//! Native OpenAI Audio API client.
//!
//! API reference:
//! - <https://developers.openai.com/api/reference/audio/createTranscription>

use crate::error::{
    ConfigurationError, DecodeError, LlmApiError, ProviderHttpError, TransportError,
};
use crate::transport::http::{join_url, normalize_base_url};
use crate::transport::{ApiResponse, HeaderSnapshot, HttpClient, HttpClientConfig};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub const API_KIND: &str = "openai:audio-transcriptions";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_AUDIO_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
pub const DEFAULT_TRANSCRIPTION_MODEL: &str = "gpt-4o-transcribe";

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
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
        let mut http = HttpClientConfig::default();
        http.request_timeout = DEFAULT_AUDIO_REQUEST_TIMEOUT;
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
            organization: None,
            project: None,
            http,
        }
    }

    pub fn from_env() -> Result<Self, LlmApiError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            ConfigurationError::new("OPENAI_API_KEY must be set for openai:audio-transcriptions")
        })?;
        if api_key.trim().is_empty() {
            return Err(ConfigurationError::new("OPENAI_API_KEY is set but empty").into());
        }
        Ok(Self::new(api_key).with_env_overrides())
    }

    pub fn from_env_allow_missing_key() -> Self {
        let mut config = match std::env::var("OPENAI_API_KEY") {
            Ok(api_key) if !api_key.trim().is_empty() => Self::new(api_key),
            _ => Self::without_api_key(),
        };
        config = config.with_env_overrides();
        config
    }

    fn with_env_overrides(mut self) -> Self {
        if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
            self.base_url = base_url;
        }
        if let Ok(organization) = std::env::var("OPENAI_ORG_ID") {
            self.organization = Some(organization);
        }
        if let Ok(project) = std::env::var("OPENAI_PROJECT_ID") {
            self.project = Some(project);
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    transcriptions_url: Url,
    auth: Option<HeaderValue>,
}

impl Client {
    pub fn new(config: Config) -> Result<Self, LlmApiError> {
        let base_url = normalize_base_url(&config.base_url)?;
        let transcriptions_url = join_url(&base_url, "audio/transcriptions")?;
        let auth = config
            .api_key
            .as_deref()
            .map(bearer_auth_value)
            .transpose()?;
        let mut headers = HeaderMap::new();
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
            transcriptions_url,
            auth,
        })
    }

    fn auth_header(
        &self,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<HeaderValue, LlmApiError> {
        match auth {
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

    pub async fn create_transcription(
        &self,
        request: CreateTranscriptionRequest,
    ) -> Result<ApiResponse<Transcription>, LlmApiError> {
        self.create_transcription_with_auth(request, None).await
    }

    pub async fn create_transcription_with_auth(
        &self,
        request: CreateTranscriptionRequest,
        auth: Option<crate::RequestAuth<'_>>,
    ) -> Result<ApiResponse<Transcription>, LlmApiError> {
        let auth = self.auth_header(auth)?;
        let file_part = reqwest::multipart::Part::bytes(request.file.bytes)
            .file_name(request.file.filename)
            .mime_str(&request.file.mime)
            .map_err(|err| ConfigurationError::new(format!("invalid audio MIME: {err}")))?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", request.model);
        if let Some(response_format) = request.response_format {
            form = form.text("response_format", response_format.as_str().to_owned());
        }
        if let Some(language) = request.language {
            form = form.text("language", language);
        }
        if let Some(prompt) = request.prompt {
            form = form.text("prompt", prompt);
        }

        let response = self
            .http
            .request(Method::POST, self.transcriptions_url.clone())
            .header(AUTHORIZATION, auth)
            .multipart(form)
            .send()
            .await
            .map_err(|err| map_reqwest_error(err, self.http.config().request_timeout))?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let body = response
            .text()
            .await
            .map_err(|err| map_reqwest_error(err, self.http.config().request_timeout))?;
        parse_json_response(status, headers, body, "OpenAI audio transcription")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFile {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub mime: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateTranscriptionRequest {
    pub file: AudioFile,
    pub model: String,
    pub response_format: Option<TranscriptionResponseFormat>,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

impl CreateTranscriptionRequest {
    pub fn new(file: AudioFile) -> Self {
        Self {
            file,
            model: DEFAULT_TRANSCRIPTION_MODEL.to_owned(),
            response_format: Some(TranscriptionResponseFormat::Json),
            language: None,
            prompt: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptionResponseFormat {
    Json,
}

impl TranscriptionResponseFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcription {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn bearer_auth_value(api_key: &str) -> Result<HeaderValue, LlmApiError> {
    let mut value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|err| ConfigurationError::new(format!("invalid OpenAI API key header: {err}")))?;
    value.set_sensitive(true);
    Ok(value)
}

fn parse_json_response<T: DeserializeOwned>(
    status: StatusCode,
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
    status: StatusCode,
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

fn map_reqwest_error(err: reqwest::Error, timeout: Duration) -> LlmApiError {
    let retryable = err.is_timeout() || err.is_connect() || err.is_request();
    let message = if err.is_timeout() {
        format!("request timed out after {}", format_duration(timeout))
    } else {
        err.to_string()
    };
    TransportError::new(message, retryable).into()
}

fn format_duration(duration: Duration) -> String {
    if duration.subsec_nanos() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{duration:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_new_uses_extended_audio_timeout() {
        let config = Config::new("test-key");

        assert_eq!(config.http.request_timeout, DEFAULT_AUDIO_REQUEST_TIMEOUT);
        assert_eq!(
            config.http.connect_timeout,
            HttpClientConfig::default().connect_timeout
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_key_fails_before_provider_io() {
        let client = Client::new(Config::without_api_key()).expect("client");

        let error = client
            .create_transcription(CreateTranscriptionRequest::new(AudioFile {
                bytes: b"not audio".to_vec(),
                filename: "audio.ogg".to_owned(),
                mime: "audio/ogg".to_owned(),
            }))
            .await
            .expect_err("missing auth must fail");

        assert!(matches!(error, LlmApiError::Configuration(_)));
    }
}
