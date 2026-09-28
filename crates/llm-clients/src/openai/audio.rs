//! Native OpenAI Audio API client.
//!
//! API reference:
//! - <https://developers.openai.com/api/reference/audio/createTranscription>

use crate::error::{
    ConfigurationError, DecodeError, LlmApiError, ProviderHttpError, TransportError,
};
use crate::transport::http::{join_url, normalize_base_url};
use crate::transport::{
    ApiResponse, EndpointOverride, HeaderSnapshot, HttpClient, HttpClientConfig,
};
use reqwest::header::{AUTHORIZATION, HeaderValue};
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
        let http = HttpClientConfig {
            request_timeout: DEFAULT_AUDIO_REQUEST_TIMEOUT,
            ..HttpClientConfig::default()
        };
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
        let headers = super::config::default_headers(
            config.organization.as_deref(),
            config.project.as_deref(),
        )?;

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
            Some(crate::RequestAuth::None) => {
                Err(ConfigurationError::new("anonymous audio requests are not supported").into())
            }
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
        self.create_transcription_with_transport(request, auth, None)
            .await
    }

    pub async fn create_transcription_with_transport(
        &self,
        request: CreateTranscriptionRequest,
        auth: Option<crate::RequestAuth<'_>>,
        endpoint: Option<&EndpointOverride>,
    ) -> Result<ApiResponse<Transcription>, LlmApiError> {
        let auth = match auth {
            Some(crate::RequestAuth::None) if endpoint.is_some() => None,
            other => Some(self.auth_header(other)?),
        };
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

        let mut request_builder = self.http.request_with_endpoint(
            Method::POST,
            self.transcriptions_url.clone(),
            "audio/transcriptions",
            endpoint,
        )?;
        if let Some(auth) = auth {
            request_builder = request_builder.header(AUTHORIZATION, auth);
        }
        let mut response = request_builder
            .multipart(form)
            .send()
            .await
            .map_err(|err| map_reqwest_error(err, self.http.config().request_timeout))?;

        let status = response.status();
        let headers = HeaderSnapshot::from_headermap(response.headers());
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| map_reqwest_error(err, self.http.config().request_timeout))?
        {
            if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                return Err(
                    DecodeError::new("transcription response exceeds the 2 MiB limit").into(),
                );
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(bytes)
            .map_err(|_| DecodeError::new("transcription response is not UTF-8"))?;
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
    #[tokio::test(flavor = "current_thread")]
    async fn custom_transport_isolated_from_deployment_headers_and_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for auth in [
            crate::RequestAuth::None,
            crate::RequestAuth::Bearer("route-key"),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = EndpointOverride::from_parts(
                &format!("http://{}/custom/v1", listener.local_addr().unwrap()),
                &BTreeMap::from([("x-route".into(), "speech".into())]),
            )
            .unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let size: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break;
                        }
                    }
                }
                let body = r#"{"text":"hello"}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                String::from_utf8(bytes).unwrap()
            });
            let mut config = Config::new("deployment-key");
            config.organization = Some("deployment-org".into());
            config.project = Some("deployment-project".into());
            let client = Client::new(config).unwrap();
            let mut request = CreateTranscriptionRequest::new(AudioFile {
                bytes: b"voice".to_vec(),
                filename: "voice.ogg".into(),
                mime: "audio/ogg".into(),
            });
            request.model = "custom-speech-model".into();
            request.language = Some("de".into());
            request.prompt = Some("Names".into());
            let result = client
                .create_transcription_with_transport(request, Some(auth), Some(&endpoint))
                .await
                .unwrap();
            assert_eq!(result.parsed.text, "hello");
            let request = server.await.unwrap();
            assert!(request.starts_with("POST /custom/v1/audio/transcriptions "));
            assert!(request.contains("x-route: speech"));
            assert!(request.contains("custom-speech-model"));
            assert!(request.contains("Names"));
            assert!(!request.contains("deployment-"));
            match auth {
                crate::RequestAuth::None => {
                    assert!(!request.to_ascii_lowercase().contains("authorization:"))
                }
                _ => assert!(request.contains("Bearer route-key")),
            }
        }
    }
}
