//! Native OpenAI Responses API client.
//!
//! API reference:
//! - <https://developers.openai.com/api/reference/resources/responses>

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
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

pub const API_KIND: &str = "openai:responses";
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

    pub fn from_env() -> Result<Self, LlmApiError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            ConfigurationError::new("OPENAI_API_KEY must be set for openai:responses")
        })?;
        if api_key.trim().is_empty() {
            return Err(ConfigurationError::new("OPENAI_API_KEY is set but empty").into());
        }

        let mut config = Self::new(api_key);
        if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
            config.base_url = base_url;
        }
        if let Ok(organization) = std::env::var("OPENAI_ORG_ID") {
            config.organization = Some(organization);
        }
        if let Ok(project) = std::env::var("OPENAI_PROJECT_ID") {
            config.project = Some(project);
        }
        Ok(config)
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    responses_url: Url,
    compact_url: Url,
    input_tokens_url: Url,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, LlmApiError> {
        let base_url = normalize_base_url(&config.base_url)?;
        let responses_url = join_url(&base_url, "responses")?;
        let compact_url = join_url(&base_url, "responses/compact")?;
        let input_tokens_url = join_url(&base_url, "responses/input_tokens")?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", config.api_key)).map_err(|err| {
                ConfigurationError::new(format!("invalid OpenAI API key header: {err}"))
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
            responses_url,
            compact_url,
            input_tokens_url,
        })
    }

    pub async fn create(
        &self,
        mut request: CreateResponseRequest,
    ) -> Result<ApiResponse<Response>, LlmApiError> {
        request.stream = Some(false);
        let response = self
            .http
            .request(Method::POST, self.responses_url.clone())
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI response")
    }

    pub async fn retrieve(
        &self,
        response_id: &str,
        mut request: RetrieveResponseRequest,
    ) -> Result<ApiResponse<Response>, LlmApiError> {
        request.stream = Some(false);
        let response = self
            .http
            .request(Method::GET, self.response_url(response_id)?)
            .query(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI retrieved response")
    }

    pub async fn retrieve_stream(
        &self,
        response_id: &str,
        mut request: RetrieveResponseRequest,
    ) -> Result<ResponseStream, LlmApiError> {
        request.stream = Some(true);
        let response = self
            .http
            .request(Method::GET, self.response_url(response_id)?)
            .query(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        if !status.is_success() {
            let body = response.text().await.map_err(map_reqwest_error)?;
            return Err(parse_provider_http_error(status, headers, body).into());
        }

        Ok(ResponseStream::new(Box::pin(response.bytes_stream())))
    }

    pub async fn delete(
        &self,
        response_id: &str,
    ) -> Result<ApiResponse<DeletedResponse>, LlmApiError> {
        let response = self
            .http
            .request(Method::DELETE, self.response_url(response_id)?)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI deleted response")
    }

    pub async fn cancel(&self, response_id: &str) -> Result<ApiResponse<Response>, LlmApiError> {
        let response = self
            .http
            .request(
                Method::POST,
                self.response_subresource_url(response_id, "cancel")?,
            )
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI cancelled response")
    }

    pub async fn compact(
        &self,
        request: CompactResponseRequest,
    ) -> Result<ApiResponse<CompactResponse>, LlmApiError> {
        let response = self
            .http
            .request(Method::POST, self.compact_url.clone())
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI compact response")
    }

    pub async fn list_input_items(
        &self,
        response_id: &str,
        request: ListInputItemsRequest,
    ) -> Result<ApiResponse<ResponseItemList>, LlmApiError> {
        let response = self
            .http
            .request(
                Method::GET,
                self.response_subresource_url(response_id, "input_items")?,
            )
            .query(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI response input items")
    }

    pub async fn count_input_tokens(
        &self,
        request: CountInputTokensRequest,
    ) -> Result<ApiResponse<InputTokens>, LlmApiError> {
        let response = self
            .http
            .request(Method::POST, self.input_tokens_url.clone())
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "OpenAI response input tokens")
    }

    pub async fn stream(
        &self,
        mut request: CreateResponseRequest,
    ) -> Result<ResponseStream, LlmApiError> {
        request.stream = Some(true);
        let response = self
            .http
            .request(Method::POST, self.responses_url.clone())
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

        Ok(ResponseStream::new(Box::pin(response.bytes_stream())))
    }

    fn response_url(&self, response_id: &str) -> Result<Url, LlmApiError> {
        self.response_subresource_url(response_id, "")
    }

    fn response_subresource_url(
        &self,
        response_id: &str,
        subresource: &str,
    ) -> Result<Url, LlmApiError> {
        let response_id = response_id.trim();
        if response_id.is_empty() {
            return Err(ConfigurationError::new("response_id must not be empty").into());
        }

        let mut url = self.responses_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| ConfigurationError::new("responses URL cannot be a base"))?;
            segments.push(response_id);
            if !subresource.is_empty() {
                segments.push(subresource);
            }
        }
        Ok(url)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CreateResponseRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<ResponseInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CreateResponseRequest {
    pub fn text(model: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            input: Some(ResponseInput::Text(input.into())),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RetrieveResponseRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_obfuscation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactResponseRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<ResponseInput>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CompactResponseRequest {
    pub fn text(model: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            input: Some(ResponseInput::Text(input.into())),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListInputItemsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<ListOrder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListOrder {
    Asc,
    Desc,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CountInputTokensRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<ResponseInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CountInputTokensRequest {
    pub fn text(model: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            input: Some(ResponseInput::Text(input.into())),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseInput {
    Text(String),
    Items(Vec<ResponseInputItem>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseInputItem {
    Message(InputMessage),
    FunctionCallOutput(FunctionCallOutput),
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    Developer,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    pub role: MessageRole,
    pub content: InputMessageContent,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputMessageContent {
    Text(String),
    Parts(Vec<InputContent>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputContent {
    InputText {
        #[serde(rename = "type")]
        r#type: InputContentType,
        text: String,
    },
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputContentType {
    InputText,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionCallOutput {
    #[serde(rename = "type")]
    pub r#type: FunctionCallOutputType,
    pub call_id: String,
    pub output: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionCallOutputType {
    FunctionCallOutput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Tool {
    Function(FunctionTool),
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionTool {
    #[serde(rename = "type")]
    pub r#type: FunctionToolType,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl FunctionTool {
    pub fn new(name: impl Into<String>, parameters: Value) -> Self {
        Self {
            r#type: FunctionToolType::Function,
            name: name.into(),
            description: None,
            parameters,
            strict: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionToolType {
    Function,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(ToolChoiceMode),
    Function {
        r#type: FunctionToolType,
        name: String,
    },
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoiceMode {
    Auto,
    Required,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub created_at: Option<f64>,
    #[serde(default)]
    pub status: Option<ResponseStatus>,
    #[serde(default)]
    pub error: Option<ResponseError>,
    #[serde(default)]
    pub incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub reasoning: Option<Reasoning>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Response {
    pub fn output_text(&self) -> String {
        self.output
            .iter()
            .flat_map(|item| item.content.iter())
            .filter_map(|part| part.text.as_deref())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn function_calls(&self) -> impl Iterator<Item = FunctionCallRef<'_>> {
        self.output
            .iter()
            .filter(|item| item.r#type == "function_call")
            .filter_map(FunctionCallRef::from_item)
    }

    pub fn reasoning_summaries(&self) -> impl Iterator<Item = &str> {
        self.output
            .iter()
            .filter(|item| item.r#type == "reasoning")
            .flat_map(|item| item.summary.iter().chain(item.content.iter()))
            .filter_map(|part| part.text.as_deref())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompactResponse {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub created_at: Option<f64>,
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeletedResponse {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    pub deleted: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Completed,
    Failed,
    InProgress,
    Incomplete,
    Cancelled,
    Queued,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncompleteDetails {
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseError {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponseOutputItem {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<ResponseContent>,
    #[serde(default)]
    pub summary: Vec<ResponseContent>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponseItemList {
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub data: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub first_id: Option<String>,
    #[serde(default)]
    pub last_id: Option<String>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponseContent {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionCallRef<'a> {
    pub item_id: Option<&'a str>,
    pub call_id: Option<&'a str>,
    pub name: &'a str,
    pub arguments: &'a str,
}

impl<'a> FunctionCallRef<'a> {
    fn from_item(item: &'a ResponseOutputItem) -> Option<Self> {
        Some(Self {
            item_id: item.id.as_deref(),
            call_id: item.call_id.as_deref(),
            name: item.name.as_deref()?,
            arguments: item.arguments.as_deref().unwrap_or(""),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens_details: Option<Value>,
    #[serde(default)]
    pub output_tokens_details: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTokens {
    #[serde(default)]
    pub object: Option<String>,
    pub input_tokens: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Usage {
    pub fn reasoning_tokens(&self) -> Option<u64> {
        self.output_tokens_details
            .as_ref()
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
    }

    pub fn cached_tokens(&self) -> Option<u64> {
        self.input_tokens_details
            .as_ref()
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub sequence_number: Option<u64>,
    #[serde(default)]
    pub response: Option<Response>,
    #[serde(default)]
    pub item: Option<ResponseOutputItem>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub output_index: Option<u64>,
    #[serde(default)]
    pub content_index: Option<u64>,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub error: Option<ResponseError>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl StreamEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.r#type.as_str(),
            "response.completed"
                | "response.failed"
                | "response.incomplete"
                | "response.cancelled"
                | "error"
        )
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

pub struct ResponseStream {
    inner: ByteStream,
    parser: SseParser,
    pending: VecDeque<ApiStreamEvent<StreamEvent>>,
    done: bool,
}

impl ResponseStream {
    fn new(inner: ByteStream) -> Self {
        Self {
            inner,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            done: false,
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<ApiStreamEvent<StreamEvent>>, LlmApiError> {
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
                            format!("OpenAI stream emitted invalid UTF-8: {err}"),
                            false,
                        )
                    })?;
                    let events = self.parser.push(chunk);
                    for event in events {
                        if let Some(parsed) = parse_sse_event(event)? {
                            self.pending.push_back(parsed);
                        }
                    }
                }
                Some(Err(err)) => {
                    return Err(StreamError::new(
                        format!("OpenAI stream read failed: {err}"),
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

pub fn parse_sse_event(sse: SseEvent) -> Result<Option<ApiStreamEvent<StreamEvent>>, LlmApiError> {
    if sse.data.trim() == "[DONE]" {
        return Ok(None);
    }
    let raw_json: Value = serde_json::from_str(&sse.data).map_err(|err| {
        DecodeError::with_raw(
            format!("invalid OpenAI Responses stream event JSON: {err}"),
            sse.data.clone(),
        )
    })?;
    let mut parsed: StreamEvent = serde_json::from_value(raw_json.clone()).map_err(|err| {
        DecodeError::with_raw(
            format!("OpenAI Responses stream event has unexpected shape: {err}"),
            raw_json.to_string(),
        )
    })?;
    if parsed.r#type.is_empty()
        && let Some(event_name) = &sse.event
    {
        parsed.r#type = event_name.clone();
    }
    Ok(Some(ApiStreamEvent::new(parsed, sse, Some(raw_json))))
}

fn map_reqwest_error(err: reqwest::Error) -> LlmApiError {
    TransportError::new(err.to_string(), err.is_connect() || err.is_request()).into()
}

fn parse_json_response<T: DeserializeOwned>(
    status: reqwest::StatusCode,
    headers: HeaderSnapshot,
    body: String,
    context: &str,
) -> Result<ApiResponse<T>, LlmApiError> {
    if !status.is_success() {
        return Err(parse_provider_http_error(status, headers, body).into());
    }

    let raw_json: Value = serde_json::from_str(&body)
        .map_err(|err| DecodeError::with_raw(format!("invalid OpenAI JSON: {err}"), body))?;
    let parsed: T = serde_json::from_value(raw_json.clone()).map_err(|err| {
        DecodeError::with_raw(
            format!("{context} did not match expected shape: {err}"),
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
    use serde_json::json;

    #[test]
    fn response_helpers_extract_output_text_usage_and_function_calls() {
        let response: Response = serde_json::from_value(json!({
            "id": "resp_1",
            "object": "response",
            "status": "completed",
            "output": [
                {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Hello" }]
                },
                {
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Zurich\"}"
                }
            ],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 4,
                "total_tokens": 7,
                "output_tokens_details": { "reasoning_tokens": 2 }
            }
        }))
        .expect("response");

        assert_eq!(response.output_text(), "Hello");
        assert_eq!(
            response.usage.as_ref().and_then(Usage::reasoning_tokens),
            Some(2)
        );
        let calls = response.function_calls().collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].call_id, Some("call_1"));
    }

    #[test]
    fn parse_sse_event_uses_raw_json_and_event_type() {
        let sse = SseEvent {
            event: Some("response.output_text.delta".to_string()),
            data: r#"{"type":"response.output_text.delta","delta":"hi","sequence_number":1}"#
                .to_string(),
            id: None,
            retry: None,
        };

        let parsed = parse_sse_event(sse).expect("parse").expect("event");

        assert_eq!(parsed.parsed.r#type, "response.output_text.delta");
        assert_eq!(parsed.parsed.delta.as_deref(), Some("hi"));
        assert_eq!(
            parsed
                .raw_json
                .as_ref()
                .and_then(|raw| raw.get("delta"))
                .and_then(Value::as_str),
            Some("hi")
        );
    }
}
