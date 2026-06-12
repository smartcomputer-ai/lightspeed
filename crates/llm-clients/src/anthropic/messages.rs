//! Native Anthropic Messages API client.
//!
//! API reference:
//! - <https://platform.claude.com/docs/en/api/messages/create>
//! - <https://platform.claude.com/docs/en/api/messages/count_tokens>
//! - <https://platform.claude.com/docs/en/build-with-claude/streaming>

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

pub const API_KIND: &str = "anthropic:messages";
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta header required when authenticating with an OAuth bearer token
/// instead of an API key.
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Default API key for every request. `None` builds a client that can
    /// only send requests carrying a per-request key.
    pub api_key: Option<String>,
    pub base_url: String,
    pub anthropic_version: String,
    pub beta_headers: Vec<String>,
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
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            beta_headers: Vec::new(),
            http: HttpClientConfig::default(),
        }
    }

    pub fn from_env() -> Result<Self, LlmApiError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            ConfigurationError::new("ANTHROPIC_API_KEY must be set for anthropic:messages")
        })?;
        if api_key.trim().is_empty() {
            return Err(ConfigurationError::new("ANTHROPIC_API_KEY is set but empty").into());
        }
        Ok(Self::new(api_key).with_env_overrides())
    }

    /// Like [`Config::from_env`], but tolerates a missing or empty
    /// `ANTHROPIC_API_KEY`: requests must then carry a per-request key.
    pub fn from_env_allow_missing_key() -> Self {
        let config = match std::env::var("ANTHROPIC_API_KEY") {
            Ok(api_key) if !api_key.trim().is_empty() => Self::new(api_key),
            _ => Self::without_api_key(),
        };
        config.with_env_overrides()
    }

    fn with_env_overrides(mut self) -> Self {
        if let Ok(base_url) = std::env::var("ANTHROPIC_BASE_URL") {
            self.base_url = base_url;
        }
        if let Ok(version) = std::env::var("ANTHROPIC_VERSION") {
            self.anthropic_version = version;
        }
        if let Ok(beta_headers) = std::env::var("ANTHROPIC_BETA") {
            self.beta_headers = split_beta_headers(&beta_headers);
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    messages_url: Url,
    count_tokens_url: Url,
    /// Configured `x-api-key` value; requests may override it with
    /// per-request auth, and fail before I/O when neither exists.
    auth: Option<HeaderValue>,
    /// Configured beta headers, kept so OAuth bearer requests can merge the
    /// OAuth beta into them instead of replacing the default header.
    beta_headers: Vec<String>,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, LlmApiError> {
        let base_url = normalize_base_url(&config.base_url)?;
        let messages_url = join_url(&base_url, "messages")?;
        let count_tokens_url = join_url(&base_url, "messages/count_tokens")?;
        let auth = config
            .api_key
            .as_deref()
            .map(api_key_header_value)
            .transpose()?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "anthropic-version",
            HeaderValue::from_str(&config.anthropic_version).map_err(|err| {
                ConfigurationError::new(format!("invalid anthropic-version header: {err}"))
            })?,
        );
        if !config.beta_headers.is_empty() {
            headers.insert(
                "anthropic-beta",
                HeaderValue::from_str(&config.beta_headers.join(",")).map_err(|err| {
                    ConfigurationError::new(format!("invalid anthropic-beta header: {err}"))
                })?,
            );
        }

        Ok(Self {
            http: HttpClient::with_headers(config.http, headers)?,
            messages_url,
            count_tokens_url,
            auth,
            beta_headers: config.beta_headers,
        })
    }

    /// Effective `x-api-key` value: the per-request key when supplied,
    /// otherwise the configured key. Fails before any I/O when neither exists.
    fn auth_header(&self, api_key: Option<&str>) -> Result<HeaderValue, LlmApiError> {
        match api_key {
            Some(api_key) => api_key_header_value(api_key),
            None => self.auth.clone().ok_or_else(|| {
                ConfigurationError::new(
                    "no Anthropic API key configured for this client and no per-request auth provided",
                )
                .into()
            }),
        }
    }

    /// Attach per-request auth: API keys go in `x-api-key`; OAuth bearer
    /// tokens go in `Authorization` plus the OAuth beta header merged with
    /// the configured beta headers (a per-request header replaces the
    /// default, so the merge must re-include them).
    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<reqwest::RequestBuilder, LlmApiError> {
        match auth {
            None | Some(crate::RequestAuth::ApiKey(_)) => {
                let api_key = match auth {
                    Some(crate::RequestAuth::ApiKey(api_key)) => Some(api_key),
                    _ => None,
                };
                Ok(builder.header("x-api-key", self.auth_header(api_key)?))
            }
            Some(crate::RequestAuth::Bearer(token)) => {
                let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|err| {
                        ConfigurationError::new(format!(
                            "invalid Anthropic bearer token header: {err}"
                        ))
                    })?;
                bearer.set_sensitive(true);
                let mut betas = self.beta_headers.clone();
                betas.push(ANTHROPIC_OAUTH_BETA.to_owned());
                let betas = HeaderValue::from_str(&betas.join(",")).map_err(|err| {
                    ConfigurationError::new(format!("invalid anthropic-beta header: {err}"))
                })?;
                Ok(builder
                    .header(AUTHORIZATION, bearer)
                    .header("anthropic-beta", betas))
            }
        }
    }

    pub async fn create(
        &self,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse<Message>, LlmApiError> {
        self.create_with_auth(request, None).await
    }

    pub async fn create_with_auth(
        &self,
        mut request: CreateMessageRequest,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<ApiResponse<Message>, LlmApiError> {
        request.stream = Some(false);
        let builder = self
            .http
            .request(Method::POST, self.messages_url.clone());
        let response = self
            .apply_auth(builder, auth)?
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "Anthropic message")
    }

    pub async fn stream(
        &self,
        mut request: CreateMessageRequest,
    ) -> Result<MessageStream, LlmApiError> {
        request.stream = Some(true);
        let response = self
            .http
            .request(Method::POST, self.messages_url.clone())
            .header("x-api-key", self.auth_header(None)?)
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

        Ok(MessageStream::new(Box::pin(response.bytes_stream())))
    }

    pub async fn count_tokens(
        &self,
        request: CountTokensRequest,
    ) -> Result<ApiResponse<CountTokensResponse>, LlmApiError> {
        let response = self
            .http
            .request(Method::POST, self.count_tokens_url.clone())
            .header("x-api-key", self.auth_header(None)?)
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response.text().await.map_err(map_reqwest_error)?;
        parse_json_response(status, headers, body, "Anthropic count tokens response")
    }
}

fn api_key_header_value(api_key: &str) -> Result<HeaderValue, LlmApiError> {
    let mut value = HeaderValue::from_str(api_key).map_err(|err| {
        ConfigurationError::new(format!("invalid Anthropic API key header: {err}"))
    })?;
    value.set_sensitive(true);
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    pub model: String,
    pub max_tokens: u64,
    pub messages: Vec<MessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    /// Output/effort configuration used with adaptive thinking models
    /// (e.g. `{"effort": "high"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CreateMessageRequest {
    pub fn user_text(model: impl Into<String>, text: impl Into<String>, max_tokens: u64) -> Self {
        Self {
            model: model.into(),
            max_tokens,
            messages: vec![MessageParam::user(text)],
            system: None,
            metadata: None,
            stop_sequences: None,
            stream: None,
            temperature: None,
            thinking: None,
            output_config: None,
            tool_choice: None,
            tools: None,
            top_k: None,
            top_p: None,
            service_tier: None,
            container: None,
            mcp_servers: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CountTokensRequest {
    pub model: String,
    pub messages: Vec<MessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl CountTokensRequest {
    pub fn user_text(model: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: vec![MessageParam::user(text)],
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<ContentBlockParam>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: MessageRole,
    pub content: MessageParamContent,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl MessageParam {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: MessageParamContent::Text(text.into()),
            extra: BTreeMap::new(),
        }
    }

    pub fn assistant(blocks: Vec<ContentBlockParam>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: MessageParamContent::Blocks(blocks),
            extra: BTreeMap::new(),
        }
    }

    pub fn user_blocks(blocks: Vec<ContentBlockParam>) -> Self {
        Self {
            role: MessageRole::User,
            content: MessageParamContent::Blocks(blocks),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageParamContent {
    Text(String),
    Blocks(Vec<ContentBlockParam>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentBlockParam {
    Text(TextBlockParam),
    Image(ImageBlockParam),
    Document(DocumentBlockParam),
    ToolUse(ToolUseBlockParam),
    ToolResult(ToolResultBlockParam),
    Thinking(ThinkingBlockParam),
    RedactedThinking(RedactedThinkingBlockParam),
    Raw(Value),
}

impl ContentBlockParam {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextBlockParam {
            r#type: "text".to_string(),
            text: text.into(),
            cache_control: None,
            extra: BTreeMap::new(),
        })
    }

    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image(ImageBlockParam {
            r#type: "image".to_string(),
            source: ImageSourceParam {
                r#type: "base64".to_string(),
                media_type: media_type.into(),
                data: data.into(),
            },
            cache_control: None,
            extra: BTreeMap::new(),
        })
    }

    /// A base64 document block (PDF).
    pub fn document_base64(
        media_type: impl Into<String>,
        data: impl Into<String>,
        title: Option<String>,
    ) -> Self {
        Self::Document(DocumentBlockParam {
            r#type: "document".to_string(),
            source: DocumentSourceParam {
                r#type: "base64".to_string(),
                media_type: media_type.into(),
                data: data.into(),
            },
            title,
            cache_control: None,
            extra: BTreeMap::new(),
        })
    }

    /// A plain-text document block; the API requires `text/plain` for text
    /// sources, so markdown/CSV content is carried as plain text.
    pub fn document_text(data: impl Into<String>, title: Option<String>) -> Self {
        Self::Document(DocumentBlockParam {
            r#type: "document".to_string(),
            source: DocumentSourceParam {
                r#type: "text".to_string(),
                media_type: "text/plain".to_string(),
                data: data.into(),
            },
            title,
            cache_control: None,
            extra: BTreeMap::new(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub source: DocumentSourceParam,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentSourceParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub source: ImageSourceParam,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageSourceParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolUseBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: String,
    pub name: String,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResultBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub tool_use_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub thinking: String,
    pub signature: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedactedThinkingBlockParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub data: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Tool {
    Custom(ToolDefinition),
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            input_schema,
            cache_control: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolChoice {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_parallel_tool_use: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ToolChoice {
    pub fn auto() -> Self {
        Self::mode("auto")
    }

    pub fn any() -> Self {
        Self::mode("any")
    }

    pub fn none() -> Self {
        Self::mode("none")
    }

    pub fn tool(name: impl Into<String>) -> Self {
        Self {
            r#type: "tool".to_string(),
            name: Some(name.into()),
            disable_parallel_tool_use: None,
            extra: BTreeMap::new(),
        }
    }

    fn mode(r#type: impl Into<String>) -> Self {
        Self {
            r#type: r#type.into(),
            name: None,
            disable_parallel_tool_use: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Thinking {
    pub fn enabled(budget_tokens: u64) -> Self {
        Self {
            r#type: "enabled".to_string(),
            budget_tokens: Some(budget_tokens),
            display: None,
            extra: BTreeMap::new(),
        }
    }

    /// Adaptive thinking for models that control thinking through
    /// `output_config.effort` instead of a token budget.
    pub fn adaptive() -> Self {
        Self {
            r#type: "adaptive".to_string(),
            budget_tokens: None,
            display: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Message {
    pub fn output_text(&self) -> String {
        self.content
            .iter()
            .filter(|block| block.r#type == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = ToolUseRef<'_>> {
        self.content
            .iter()
            .filter(|block| block.r#type == "tool_use")
            .filter_map(ToolUseRef::from_block)
    }

    pub fn thinking_blocks(&self) -> impl Iterator<Item = &ContentBlock> {
        self.content
            .iter()
            .filter(|block| block.r#type == "thinking" || block.r#type == "redacted_thinking")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub is_error: Option<bool>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolUseRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub input: Option<&'a Value>,
}

impl<'a> ToolUseRef<'a> {
    fn from_block(block: &'a ContentBlock) -> Option<Self> {
        Some(Self {
            id: block.id.as_deref()?,
            name: block.name.as_deref()?,
            input: block.input.as_ref(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    ModelContextWindow,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub server_tool_use: Option<Value>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Usage {
    pub fn total_tokens(&self) -> Option<u64> {
        Some(self.input_tokens? + self.output_tokens?)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CountTokensResponse {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub index: Option<u64>,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub content_block: Option<ContentBlock>,
    #[serde(default)]
    pub delta: Option<StreamDelta>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub error: Option<AnthropicError>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl StreamEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(self.r#type.as_str(), "message_stop" | "error")
    }

    pub fn text_delta(&self) -> Option<&str> {
        self.delta
            .as_ref()
            .filter(|delta| delta.r#type == "text_delta")
            .and_then(|delta| delta.text.as_deref())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamDelta {
    #[serde(rename = "type")]
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub partial_json: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicError {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

pub struct MessageStream {
    inner: ByteStream,
    parser: SseParser,
    pending: VecDeque<ApiStreamEvent<StreamEvent>>,
    done: bool,
}

impl MessageStream {
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
                            format!("Anthropic stream emitted invalid UTF-8: {err}"),
                            false,
                        )
                    })?;
                    let events = self.parser.push(chunk);
                    for event in events {
                        let parsed = parse_sse_event(event)?;
                        self.pending.push_back(parsed);
                    }
                }
                Some(Err(err)) => {
                    return Err(StreamError::new(
                        format!("Anthropic stream read failed: {err}"),
                        true,
                    )
                    .into());
                }
                None => {
                    self.done = true;
                    if let Some(event) = std::mem::take(&mut self.parser).finish() {
                        let parsed = parse_sse_event(event)?;
                        self.pending.push_back(parsed);
                    }
                }
            }
        }
    }
}

pub fn parse_sse_event(sse: SseEvent) -> Result<ApiStreamEvent<StreamEvent>, LlmApiError> {
    let raw_json: Value = serde_json::from_str(&sse.data).map_err(|err| {
        DecodeError::with_raw(
            format!("invalid Anthropic stream event JSON: {err}"),
            sse.data.clone(),
        )
    })?;
    let mut parsed: StreamEvent = serde_json::from_value(raw_json.clone()).map_err(|err| {
        DecodeError::with_raw(
            format!("Anthropic stream event has unexpected shape: {err}"),
            raw_json.to_string(),
        )
    })?;
    if parsed.r#type.is_empty()
        && let Some(event_name) = &sse.event
    {
        parsed.r#type = event_name.clone();
    }
    Ok(ApiStreamEvent::new(parsed, sse, Some(raw_json)))
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
        .map_err(|err| DecodeError::with_raw(format!("invalid Anthropic JSON: {err}"), body))?;
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
        None,
        error_type,
        Some(message),
        raw_json,
        Some(body),
    )
}

fn split_beta_headers(value: &str) -> Vec<String> {
    value
        .split([',', ' '])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_helpers_extract_text_usage_and_tool_uses() {
        let message: Message = serde_json::from_value(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "Checking" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": { "city": "Zurich" }
                }
            ],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 4,
                "cache_read_input_tokens": 2
            }
        }))
        .expect("message");

        assert_eq!(message.output_text(), "Checking");
        assert_eq!(message.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(
            message.usage.as_ref().and_then(Usage::total_tokens),
            Some(7)
        );
        let tools = message.tool_uses().collect::<Vec<_>>();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(
            tools[0].input.and_then(|input| input.get("city")),
            Some(&json!("Zurich"))
        );
    }

    #[test]
    fn parse_sse_event_preserves_raw_json_and_event_type() {
        let sse = SseEvent {
            event: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#
                .to_string(),
            id: None,
            retry: None,
        };

        let parsed = parse_sse_event(sse).expect("parse");

        assert_eq!(parsed.parsed.r#type, "content_block_delta");
        assert_eq!(parsed.parsed.text_delta(), Some("hi"));
        assert_eq!(
            parsed
                .raw_json
                .as_ref()
                .and_then(|raw| raw.get("delta"))
                .and_then(|delta| delta.get("text"))
                .and_then(Value::as_str),
            Some("hi")
        );
    }
}
