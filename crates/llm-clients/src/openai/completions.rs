//! Native OpenAI Chat Completions-compatible API client.
//!
//! API reference:
//! - <https://developers.openai.com/api/reference/resources/chat/subresources/completions>

use crate::error::{
    ConfigurationError, DecodeError, LlmApiError, ProviderHttpError, StreamError, TransportError,
};
use crate::transport::http::{join_url, normalize_base_url};
use crate::transport::{ApiResponse, ApiStreamEvent, HeaderSnapshot, HttpClient, HttpClientConfig};
use crate::{SseEvent, SseParser};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

pub const API_KIND: &str = "openai:completions";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub organization: Option<String>,
    pub project: Option<String>,
    pub http: HttpClientConfig,
}

impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            organization: None,
            project: None,
            http: HttpClientConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    completions_url: Url,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, LlmApiError> {
        let base_url = normalize_base_url(&config.base_url)?;
        let completions_url = join_url(&base_url, "chat/completions")?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", config.api_key)).map_err(|err| {
                ConfigurationError::new(format!("invalid OpenAI-compatible API key header: {err}"))
            })?,
        );
        if let Some(organization) = &config.organization {
            headers.insert(
                "OpenAI-Organization",
                HeaderValue::from_str(organization).map_err(|err| {
                    ConfigurationError::new(format!("invalid OpenAI organization header: {err}"))
                })?,
            );
        }
        if let Some(project) = &config.project {
            headers.insert(
                "OpenAI-Project",
                HeaderValue::from_str(project).map_err(|err| {
                    ConfigurationError::new(format!("invalid OpenAI project header: {err}"))
                })?,
            );
        }

        Ok(Self {
            http: HttpClient::with_headers(config.http, headers)?,
            completions_url,
        })
    }

    pub async fn create(
        &self,
        mut request: CreateCompletionRequest,
    ) -> Result<ApiResponse<Completion>, LlmApiError> {
        request.stream = Some(false);
        let response = self
            .http
            .request(Method::POST, self.completions_url.clone())
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        if !status.is_success() {
            return Err(parse_provider_http_error(status, headers, body).into());
        }

        let raw_json: Value = serde_json::from_str(&body).map_err(|err| {
            DecodeError::with_raw(
                format!("invalid OpenAI completions JSON: {err}"),
                body.clone(),
            )
        })?;
        let parsed: Completion = serde_json::from_value(raw_json.clone()).map_err(|err| {
            DecodeError::with_raw(
                format!("OpenAI completion did not match expected shape: {err}"),
                raw_json.to_string(),
            )
        })?;
        Ok(ApiResponse::new(parsed, raw_json, status, headers))
    }

    pub async fn stream(
        &self,
        mut request: CreateCompletionRequest,
    ) -> Result<CompletionStream, LlmApiError> {
        request.stream = Some(true);
        if request.stream_options.is_none() {
            request.stream_options = Some(StreamOptions {
                include_usage: Some(true),
                include_obfuscation: None,
                extra: BTreeMap::new(),
            });
        }

        let response = self
            .http
            .request(Method::POST, self.completions_url.clone())
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        if !status.is_success() {
            let body = response.text().await.map_err(map_reqwest_error)?;
            return Err(parse_provider_http_error(status, headers, body).into());
        }

        Ok(CompletionStream::new(Box::pin(response.bytes_stream())))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CreateCompletionRequest {
    pub model: String,
    pub messages: Vec<CompletionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<CompletionTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<CompletionToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CreateCompletionRequest {
    pub fn user_text(model: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: vec![CompletionMessage::user(text)],
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_obfuscation: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<CompletionMessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CompletionToolCall>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CompletionMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(CompletionMessageContent::Text(text.into())),
            ..Self::default()
        }
    }

    pub fn text(&self) -> String {
        match &self.content {
            Some(CompletionMessageContent::Text(text)) => text.clone(),
            Some(CompletionMessageContent::Parts(parts)) => parts
                .iter()
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join(""),
            None => String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompletionMessageContent {
    Text(String),
    Parts(Vec<CompletionContent>),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionContent {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompletionTool {
    #[serde(rename = "type")]
    pub r#type: CompletionToolType,
    pub function: CompletionFunction,
}

impl CompletionTool {
    pub fn function(name: impl Into<String>, parameters: Value) -> Self {
        Self {
            r#type: CompletionToolType::Function,
            function: CompletionFunction {
                name: name.into(),
                description: None,
                parameters: Some(parameters),
                strict: None,
                extra: BTreeMap::new(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionToolType {
    Function,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionFunction {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompletionToolChoice {
    Mode(CompletionToolChoiceMode),
    Function {
        r#type: CompletionToolType,
        function: CompletionToolChoiceFunction,
    },
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionToolChoiceMode {
    Auto,
    Required,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionToolChoiceFunction {
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<CompletionChoice>,
    #[serde(default)]
    pub usage: Option<CompletionUsage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Completion {
    pub fn output_text(&self) -> String {
        self.choices
            .iter()
            .filter_map(|choice| choice.message.as_ref())
            .map(CompletionMessage::text)
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = CompletionToolCallRef<'_>> {
        self.choices
            .iter()
            .filter_map(|choice| choice.message.as_ref())
            .flat_map(|message| message.tool_calls.iter().flatten())
            .filter_map(CompletionToolCallRef::from_call)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionChoice {
    pub index: u64,
    #[serde(default)]
    pub message: Option<CompletionMessage>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub logprobs: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionToolCall {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub function: Option<CompletionFunctionCall>,
    #[serde(default)]
    pub index: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionToolCallRef<'a> {
    pub id: Option<&'a str>,
    pub name: &'a str,
    pub arguments: &'a str,
}

impl<'a> CompletionToolCallRef<'a> {
    fn from_call(call: &'a CompletionToolCall) -> Option<Self> {
        let function = call.function.as_ref()?;
        Some(Self {
            id: call.id.as_deref(),
            name: function.name.as_deref()?,
            arguments: function.arguments.as_deref().unwrap_or(""),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_tokens_details: Option<Value>,
    #[serde(default)]
    pub completion_tokens_details: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CompletionUsage {
    pub fn reasoning_tokens(&self) -> Option<u64> {
        self.completion_tokens_details
            .as_ref()
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
    }

    pub fn cached_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionChunk {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<CompletionChunkChoice>,
    #[serde(default)]
    pub usage: Option<CompletionUsage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CompletionChunk {
    pub fn is_terminal(&self) -> bool {
        self.choices
            .iter()
            .any(|choice| choice.finish_reason.is_some())
            || (self.choices.is_empty() && self.usage.is_some())
    }

    pub fn text_delta(&self) -> String {
        self.choices
            .iter()
            .filter_map(|choice| choice.delta.as_ref())
            .filter_map(|delta| delta.content.as_deref())
            .collect::<Vec<_>>()
            .join("")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionChunkChoice {
    pub index: u64,
    #[serde(default)]
    pub delta: Option<CompletionDelta>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub logprobs: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<CompletionToolCall>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

pub struct CompletionStream {
    inner: ByteStream,
    parser: SseParser,
    pending: VecDeque<ApiStreamEvent<CompletionChunk>>,
    done: bool,
}

impl CompletionStream {
    fn new(inner: ByteStream) -> Self {
        Self {
            inner,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            done: false,
        }
    }

    pub async fn next_chunk(
        &mut self,
    ) -> Result<Option<ApiStreamEvent<CompletionChunk>>, LlmApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            if self.done {
                return Ok(None);
            }

            match self.inner.next().await {
                Some(Ok(bytes)) => {
                    let chunk = std::str::from_utf8(&bytes).map_err(|err| {
                        StreamError::new(
                            format!("OpenAI completions stream emitted invalid UTF-8: {err}"),
                            false,
                        )
                    })?;
                    for event in self.parser.push(chunk) {
                        if let Some(parsed) = parse_sse_event(event)? {
                            self.pending.push_back(parsed);
                        }
                    }
                }
                Some(Err(err)) => {
                    return Err(StreamError::new(
                        format!("OpenAI completions stream read failed: {err}"),
                        true,
                    )
                    .into());
                }
                None => {
                    self.done = true;
                    if let Some(event) = std::mem::take(&mut self.parser).finish()
                        && let Some(parsed) = parse_sse_event(event)?
                    {
                        self.pending.push_back(parsed);
                    }
                }
            }
        }
    }
}

pub fn parse_sse_event(
    sse: SseEvent,
) -> Result<Option<ApiStreamEvent<CompletionChunk>>, LlmApiError> {
    if sse.data.trim() == "[DONE]" {
        return Ok(None);
    }
    let raw_json: Value = serde_json::from_str(&sse.data).map_err(|err| {
        DecodeError::with_raw(
            format!("invalid OpenAI completions stream chunk JSON: {err}"),
            sse.data.clone(),
        )
    })?;
    let parsed: CompletionChunk = serde_json::from_value(raw_json.clone()).map_err(|err| {
        DecodeError::with_raw(
            format!("OpenAI completions stream chunk has unexpected shape: {err}"),
            raw_json.to_string(),
        )
    })?;
    Ok(Some(ApiStreamEvent::new(parsed, sse, Some(raw_json))))
}

fn map_reqwest_error(err: reqwest::Error) -> LlmApiError {
    TransportError::new(err.to_string(), err.is_connect() || err.is_request()).into()
}

fn parse_provider_http_error(
    status: reqwest::StatusCode,
    headers: HeaderSnapshot,
    body: String,
) -> ProviderHttpError {
    let raw_json = serde_json::from_str::<Value>(&body).ok();
    let error = raw_json.as_ref().and_then(|value| value.get("error"));
    let error_code = error
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_type = error
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| body.clone());

    ProviderHttpError::new(API_KIND, status, message.clone(), headers).with_provider_details(
        error_code,
        error_type,
        Some(message),
        raw_json,
        Some(body),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completion_helpers_extract_text_usage_and_tool_calls() {
        let completion: Completion = serde_json::from_value(json!({
            "id": "chatcmpl_1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": "hello",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Zurich\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 4,
                "total_tokens": 7,
                "completion_tokens_details": { "reasoning_tokens": 1 }
            }
        }))
        .expect("completion");

        assert_eq!(completion.output_text(), "hello");
        assert_eq!(
            completion
                .usage
                .as_ref()
                .and_then(CompletionUsage::reasoning_tokens),
            Some(1)
        );
        let calls = completion.tool_calls().collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }

    #[test]
    fn parse_sse_event_preserves_raw_json_and_text_delta() {
        let sse = SseEvent {
            event: None,
            data: r#"{"id":"chatcmpl_1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#.to_string(),
            id: None,
            retry: None,
        };

        let parsed = parse_sse_event(sse).expect("parse").expect("event");

        assert_eq!(parsed.parsed.text_delta(), "hi");
        assert_eq!(
            parsed
                .raw_json
                .as_ref()
                .and_then(|raw| raw.get("id"))
                .and_then(Value::as_str),
            Some("chatcmpl_1")
        );
    }
}
