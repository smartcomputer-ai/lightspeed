//! Native OpenAI Chat Completions-compatible API client.
//!
//! API reference:
//! - <https://developers.openai.com/api/reference/resources/chat/subresources/completions>

use crate::error::{
    ConfigurationError, DecodeError, LlmApiError, ProviderHttpError, StreamError, TransportError,
};
use crate::transport::http::{join_url, normalize_base_url};
use crate::transport::{
    ApiResponse, ApiStreamEvent, EndpointOverride, HeaderSnapshot, HttpClient, HttpClientConfig,
};
use crate::{SseEvent, SseParser};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

pub const API_KIND: &str = "openai:completions";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Default API key for every request. `None` builds a client that can
    /// only send requests carrying a per-request key.
    pub api_key: Option<String>,
    pub base_url: String,
    pub organization: Option<String>,
    pub project: Option<String>,
    pub http: HttpClientConfig,
}

impl Config {
    pub fn new(api_key: impl Into<String>) -> Self {
        let mut config = Self::without_api_key();
        config.api_key = Some(api_key.into());
        config
    }

    pub fn without_api_key() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            organization: None,
            project: None,
            http: HttpClientConfig::default(),
        }
    }

    pub fn from_env() -> Result<Self, LlmApiError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            ConfigurationError::new("OPENAI_API_KEY must be set for openai:completions")
        })?;
        if api_key.trim().is_empty() {
            return Err(ConfigurationError::new("OPENAI_API_KEY is set but empty").into());
        }
        Ok(Self::new(api_key).with_env_overrides())
    }

    /// Like [`Config::from_env`], but tolerates a missing or empty key so
    /// requests can authenticate through a universe-owned provider row.
    pub fn from_env_allow_missing_key() -> Self {
        let mut config = match std::env::var("OPENAI_API_KEY") {
            Ok(api_key) if !api_key.trim().is_empty() => Self::new(api_key),
            _ => Self::without_api_key(),
        };
        config = config.with_env_overrides();
        config
    }

    fn with_env_overrides(mut self) -> Self {
        super::config::apply_env_overrides(
            &mut self.base_url,
            &mut self.organization,
            &mut self.project,
            |name| std::env::var(name).ok(),
        );
        self
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    completions_url: Url,
    models_url: Url,
    auth: Option<HeaderValue>,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, LlmApiError> {
        let base_url = normalize_base_url(&config.base_url)?;
        let completions_url = join_url(&base_url, "chat/completions")?;
        let models_url = join_url(&base_url, "models")?;
        let auth = config
            .api_key
            .as_deref()
            .map(bearer_auth_value)
            .transpose()?;
        let mut headers = super::config::default_headers(
            config.organization.as_deref(),
            config.project.as_deref(),
        )?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        Ok(Self {
            http: HttpClient::with_headers(config.http, headers)?,
            completions_url,
            models_url,
            auth,
        })
    }

    fn auth_header(
        &self,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<HeaderValue, LlmApiError> {
        match auth {
            Some(crate::RequestAuth::None) => Err(ConfigurationError::new(
                "anonymous auth must be sent through an explicit endpoint override",
            )
            .into()),
            Some(crate::RequestAuth::ApiKey(value)) | Some(crate::RequestAuth::Bearer(value)) => {
                bearer_auth_value(value)
            }
            None => self.auth.clone().ok_or_else(|| {
                ConfigurationError::new(
                    "no OpenAI API key configured for this client and no per-request auth provided",
                )
                .into()
            }),
        }
    }

    fn transport_auth_header(
        &self,
        auth: Option<crate::RequestAuth<'_>>,
        endpoint: Option<&EndpointOverride>,
    ) -> Result<Option<HeaderValue>, LlmApiError> {
        match auth {
            Some(crate::RequestAuth::None) if endpoint.is_some() => Ok(None),
            Some(crate::RequestAuth::None) => Err(ConfigurationError::new(
                "anonymous auth requires an explicit endpoint override",
            )
            .into()),
            other => self.auth_header(other).map(Some),
        }
    }

    pub async fn create(
        &self,
        request: CreateCompletionRequest,
    ) -> Result<ApiResponse<Completion>, LlmApiError> {
        self.create_with_auth(request, None).await
    }

    pub async fn create_with_auth(
        &self,
        request: CreateCompletionRequest,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<ApiResponse<Completion>, LlmApiError> {
        self.create_with_transport(request, auth, None).await
    }

    pub async fn create_with_transport(
        &self,
        mut request: CreateCompletionRequest,
        auth: Option<crate::RequestAuth<'_>>,
        endpoint: Option<&EndpointOverride>,
    ) -> Result<ApiResponse<Completion>, LlmApiError> {
        request.stream = Some(false);
        let auth = self.transport_auth_header(auth, endpoint)?;
        let mut request_builder = self.http.request_with_endpoint(
            Method::POST,
            self.completions_url.clone(),
            "chat/completions",
            endpoint,
        )?;
        if let Some(auth) = auth {
            request_builder = request_builder.header(AUTHORIZATION, auth);
        }
        let response = request_builder
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

    pub async fn list_models(&self) -> Result<ApiResponse<ModelList>, LlmApiError> {
        self.list_models_with_auth(None).await
    }

    pub async fn list_models_with_auth(
        &self,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<ApiResponse<ModelList>, LlmApiError> {
        self.list_models_with_transport(auth, None).await
    }

    pub async fn list_models_with_transport(
        &self,
        auth: Option<crate::RequestAuth<'_>>,
        endpoint: Option<&EndpointOverride>,
    ) -> Result<ApiResponse<ModelList>, LlmApiError> {
        let auth = self.transport_auth_header(auth, endpoint)?;
        let mut request_builder = self.http.request_with_endpoint(
            Method::GET,
            self.models_url.clone(),
            "models",
            endpoint,
        )?;
        if let Some(auth) = auth {
            request_builder = request_builder.header(AUTHORIZATION, auth);
        }
        let response = request_builder.send().await.map_err(map_reqwest_error)?;
        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI model list")
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
            .header(AUTHORIZATION, self.auth_header(None)?)
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
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Routes requests sharing a prefix to the same cache; OpenAI's prompt
    /// cache is automatic but best-effort without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
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

    /// Provider-native reasoning fields that must be replayed with an
    /// assistant message during tool loops. DeepSeek uses
    /// `reasoning_content`; OpenRouter may return `reasoning` and/or the
    /// structured `reasoning_details` array.
    pub fn reasoning_state(&self) -> BTreeMap<String, Value> {
        ["reasoning_content", "reasoning", "reasoning_details"]
            .into_iter()
            .filter_map(|key| {
                self.extra
                    .get(key)
                    .cloned()
                    .map(|value| (key.to_owned(), value))
            })
            .collect()
    }

    /// Provider-native output annotations, including OpenAI web citations.
    pub fn annotations(&self) -> Option<&Value> {
        self.extra.get("annotations")
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<CompletionFunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelList {
    #[serde(default)]
    pub data: Vec<Model>,
    #[serde(default)]
    pub object: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub created: Option<i64>,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub owned_by: Option<String>,
}

impl CompletionUsage {
    pub fn reasoning_tokens(&self) -> Option<u64> {
        self.completion_tokens_details
            .as_ref()
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
    }

    pub fn cached_tokens(&self) -> Option<u64> {
        self.extra
            .get("prompt_cache_hit_tokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                self.prompt_tokens_details
                    .as_ref()
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
    }

    /// DeepSeek reports uncached prompt tokens separately from cache hits.
    pub fn cache_miss_tokens(&self) -> Option<u64> {
        self.extra
            .get("prompt_cache_miss_tokens")
            .and_then(Value::as_u64)
    }

    /// OpenRouter reports request cost in its usage extension. The raw JSON
    /// number is returned so callers can preserve its provider precision.
    pub fn cost(&self) -> Option<&Value> {
        self.extra.get("cost")
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

fn bearer_auth_value(value: &str) -> Result<HeaderValue, LlmApiError> {
    HeaderValue::from_str(&format!("Bearer {value}")).map_err(|err| {
        ConfigurationError::new(format!("invalid OpenAI authorization header: {err}")).into()
    })
}

fn parse_json_response<T: serde::de::DeserializeOwned>(
    status: reqwest::StatusCode,
    headers: HeaderSnapshot,
    body: String,
    label: &str,
) -> Result<ApiResponse<T>, LlmApiError> {
    if !status.is_success() {
        return Err(parse_provider_http_error(status, headers, body).into());
    }
    let raw_json: Value = serde_json::from_str(&body).map_err(|err| {
        DecodeError::with_raw(format!("invalid {label} JSON: {err}"), body.clone())
    })?;
    let parsed = serde_json::from_value(raw_json.clone()).map_err(|err| {
        DecodeError::with_raw(
            format!("{label} did not match expected shape: {err}"),
            raw_json.to_string(),
        )
    })?;
    Ok(ApiResponse::new(parsed, raw_json, status, headers))
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
    use crate::RequestAuth;
    use serde_json::json;

    #[test]
    fn auth_header_prefers_per_request_auth_and_supports_bearer_tokens() {
        let client = Client::new(Config::new("deployment-key")).expect("client");

        let api_key = client
            .auth_header(Some(RequestAuth::ApiKey("universe-key")))
            .expect("api-key auth");
        let bearer = client
            .auth_header(Some(RequestAuth::Bearer("oauth-token")))
            .expect("bearer auth");
        let default = client.auth_header(None).expect("default auth");

        assert_eq!(api_key.to_str().expect("header"), "Bearer universe-key");
        assert_eq!(bearer.to_str().expect("header"), "Bearer oauth-token");
        assert_eq!(default.to_str().expect("header"), "Bearer deployment-key");
    }

    #[test]
    fn auth_header_fails_before_io_when_no_key_is_available() {
        let client = Client::new(Config::without_api_key()).expect("client");

        let error = client
            .auth_header(None)
            .expect_err("missing auth must fail");

        assert!(matches!(error, LlmApiError::Configuration(_)));
    }

    #[test]
    fn fast_service_tier_round_trips_through_request_and_response() {
        let request = CreateCompletionRequest {
            model: "gpt-5.6-sol".to_owned(),
            messages: vec![CompletionMessage::user("hello")],
            service_tier: Some("fast".to_owned()),
            ..CreateCompletionRequest::default()
        };
        let value = serde_json::to_value(request).expect("request JSON");
        assert_eq!(value["service_tier"], "fast");

        let completion: Completion = serde_json::from_value(json!({
            "id": "chatcmpl_fast",
            "service_tier": "priority",
            "choices": []
        }))
        .expect("completion");
        assert_eq!(completion.service_tier.as_deref(), Some("priority"));
    }

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
    fn compatible_extensions_preserve_reasoning_annotations_cache_and_cost() {
        let completion: Completion = serde_json::from_value(json!({
            "id": "chatcmpl_extensions",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "private chain",
                    "reasoning": "provider reasoning",
                    "reasoning_details": [{"type":"reasoning.text","text":"exact"}],
                    "annotations": [{"type":"url_citation","url_citation":{"url":"https://example.com"}}]
                }
            }],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 5,
                "total_tokens": 16,
                "prompt_cache_hit_tokens": 7,
                "prompt_cache_miss_tokens": 4,
                "cost": 0.0000123
            }
        }))
        .expect("completion");

        let message = completion.choices[0].message.as_ref().expect("message");
        assert_eq!(
            message.reasoning_state(),
            BTreeMap::from([
                ("reasoning".to_owned(), json!("provider reasoning")),
                ("reasoning_content".to_owned(), json!("private chain")),
                (
                    "reasoning_details".to_owned(),
                    json!([{"type":"reasoning.text","text":"exact"}]),
                ),
            ])
        );
        assert!(message.annotations().is_some());
        let usage = completion.usage.as_ref().expect("usage");
        assert_eq!(usage.cached_tokens(), Some(7));
        assert_eq!(usage.cache_miss_tokens(), Some(4));
        assert_eq!(usage.cost(), Some(&json!(0.0000123)));
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
