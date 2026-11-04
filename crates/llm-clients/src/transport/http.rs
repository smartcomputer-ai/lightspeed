//! HTTP client wrapper and URL helpers for provider clients.

use crate::error::{ConfigurationError, LlmApiError, TransportError};
use crate::transport::HeaderSnapshot;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, RequestBuilder, Url};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// Per-request endpoint override for an OpenAI-compatible provider. Header
/// values are deliberately absent from `Debug` output.
#[derive(Clone)]
pub struct EndpointOverride {
    base_url: Url,
    headers: HeaderMap,
}

impl EndpointOverride {
    pub fn from_parts(
        base_url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<Self, LlmApiError> {
        let base_url = normalize_base_url(base_url)?;
        validate_endpoint_url(&base_url)?;
        if headers.len() > 32 {
            return Err(ConfigurationError::new(
                "endpoint headers must contain at most 32 entries",
            )
            .into());
        }
        let mut parsed_headers = HeaderMap::new();
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                ConfigurationError::new(format!("invalid endpoint header name {name:?}: {error}"))
            })?;
            if is_reserved_endpoint_header(&name) {
                return Err(ConfigurationError::new(format!(
                    "endpoint header {name:?} is transport-owned and cannot be overridden"
                ))
                .into());
            }
            if value.len() > 4096 {
                return Err(ConfigurationError::new(format!(
                    "endpoint header {name:?} exceeds the supported value length"
                ))
                .into());
            }
            let value = HeaderValue::from_str(value).map_err(|error| {
                ConfigurationError::new(format!("invalid endpoint header value: {error}"))
            })?;
            parsed_headers.insert(name, value);
        }
        Ok(Self {
            base_url,
            headers: parsed_headers,
        })
    }

    pub fn url(&self, path: &str) -> Result<Url, LlmApiError> {
        join_url(&self.base_url, path)
    }
}

fn validate_endpoint_url(url: &Url) -> Result<(), LlmApiError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            ConfigurationError::new("endpoint base URL must not include credentials").into(),
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigurationError::new(
            "endpoint base URL must not include a query or fragment",
        )
        .into());
    }
    match url.scheme() {
        "https" => Ok(()),
        "http"
            if url.host_str().is_some_and(|host| {
                let host = host
                    .strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host);
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }) =>
        {
            Ok(())
        }
        "http" => Err(ConfigurationError::new(
            "endpoint base URL must use HTTPS; HTTP is allowed only for loopback hosts",
        )
        .into()),
        scheme => Err(ConfigurationError::new(format!(
            "unsupported endpoint base URL scheme '{scheme}'"
        ))
        .into()),
    }
}

fn is_reserved_endpoint_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "authorization"
            | "content-type"
            | "host"
            | "cookie"
            | "set-cookie"
            | "connection"
            | "transfer-encoding"
    )
}

impl std::fmt::Debug for EndpointOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointOverride")
            .field("base_url", &self.base_url)
            .field("header_count", &self.headers.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HttpClientConfig {
    pub connect_timeout: Duration,
    /// Whole-request ceiling for one provider call. Non-streaming generations
    /// with thinking run for minutes on frontier models, so this matches the
    /// official SDKs' ten-minute default; the LLM activity's start-to-close
    /// stays above it so Temporal never times out a request that is still
    /// being served.
    pub request_timeout: Duration,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(600),
        }
    }
}

#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
    default_headers: HeaderMap,
    config: HttpClientConfig,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("default_header_count", &self.default_headers.len())
            .field("config", &self.config)
            .finish()
    }
}

impl HttpClient {
    pub fn new(config: HttpClientConfig) -> Result<Self, LlmApiError> {
        Self::with_headers(config, HeaderMap::new())
    }

    pub fn with_headers(
        config: HttpClientConfig,
        default_headers: HeaderMap,
    ) -> Result<Self, LlmApiError> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|err| TransportError::new(err.to_string(), true))?;

        Ok(Self {
            client,
            default_headers,
            config,
        })
    }

    pub fn config(&self) -> HttpClientConfig {
        self.config
    }

    pub fn default_headers(&self) -> HeaderSnapshot {
        HeaderSnapshot::from_headermap(&self.default_headers)
    }

    pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let mut builder = self.client.request(method, url);
        if !self.default_headers.is_empty() {
            builder = builder.headers(self.default_headers.clone());
        }
        builder
    }

    /// Build a request against a per-provider endpoint. Overrides deliberately
    /// do not inherit the client's OpenAI organization/project defaults.
    pub fn request_with_endpoint(
        &self,
        method: Method,
        default_url: Url,
        endpoint_path: &str,
        endpoint: Option<&EndpointOverride>,
    ) -> Result<RequestBuilder, LlmApiError> {
        let Some(endpoint) = endpoint else {
            return Ok(self.request(method, default_url));
        };
        let mut builder = self.client.request(method, endpoint.url(endpoint_path)?);
        if !endpoint.headers.is_empty() {
            builder = builder.headers(endpoint.headers.clone());
        }
        Ok(builder)
    }

    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.default_headers.insert(name, value);
        self
    }

    /// The transport failure for one failed exchange, the same for every
    /// provider client. See [`transport_error`].
    pub fn transport_error(&self, err: reqwest::Error) -> LlmApiError {
        transport_error(err, self.config.request_timeout)
    }
}

/// Classifies a failed exchange with the provider. A timeout, a failed connect
/// and a failed send may succeed on a resend. So may a failed body read:
/// reqwest reports a connection reset or a truncated body after the headers as
/// a decode error, not a body error. What stays terminal is a request reqwest
/// refused to build or follow, which would fail the same way again.
fn transport_error(err: reqwest::Error, request_timeout: Duration) -> LlmApiError {
    let retryable = err.is_timeout() || err.is_connect() || err.is_request() || err.is_decode();
    let message = if err.is_timeout() {
        format!(
            "request timed out after {}: {err}",
            format_duration(request_timeout)
        )
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

pub fn normalize_base_url(base_url: &str) -> Result<Url, LlmApiError> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(ConfigurationError::new("base URL must not be empty").into());
    }

    let mut url = Url::parse(trimmed)
        .map_err(|err| ConfigurationError::new(format!("invalid base URL '{trimmed}': {err}")))?;

    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(
                ConfigurationError::new(format!("unsupported base URL scheme '{scheme}'")).into(),
            );
        }
    }

    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

pub fn join_url(base_url: &Url, path: &str) -> Result<Url, LlmApiError> {
    let path = path.strip_prefix('/').unwrap_or(path);
    base_url.join(path).map_err(|err| {
        ConfigurationError::new(format!("invalid request path '{path}': {err}")).into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn a_request_reqwest_refuses_to_build_is_terminal() {
        let client = reqwest::Client::new();
        let err = client.get("not a url").build().expect_err("invalid URL");
        let error = transport_error(err, Duration::from_secs(1));
        assert!(matches!(error, LlmApiError::Transport(_)));
        assert!(!error.retryable());
    }

    #[test]
    fn timeout_duration_formats_as_seconds() {
        assert_eq!(format_duration(Duration::from_secs(300)), "300s");
    }

    #[test]
    fn normalize_base_url_requires_http_scheme() {
        let err = normalize_base_url("file:///tmp").expect_err("unsupported scheme");
        assert!(matches!(err, LlmApiError::Configuration(_)));
    }

    #[test]
    fn normalize_base_url_adds_trailing_slash() {
        let url = normalize_base_url("https://api.example.test/v1").expect("url");
        assert_eq!(url.as_str(), "https://api.example.test/v1/");
    }

    #[test]
    fn join_url_uses_normalized_base_path() {
        let base = normalize_base_url("https://api.example.test/v1").expect("url");
        let url = join_url(&base, "/responses").expect("joined");
        assert_eq!(url.as_str(), "https://api.example.test/v1/responses");
    }

    #[test]
    fn http_client_debug_does_not_print_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        let client =
            HttpClient::with_headers(HttpClientConfig::default(), headers).expect("client");
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("default_header_count"));
    }

    #[test]
    fn endpoint_override_debug_redacts_header_values_and_joins_paths() {
        let endpoint = EndpointOverride::from_parts(
            "https://router.example/v1",
            &BTreeMap::from([("x-title".to_owned(), "sensitive-ish".to_owned())]),
        )
        .expect("endpoint");
        assert_eq!(
            endpoint.url("chat/completions").expect("URL").as_str(),
            "https://router.example/v1/chat/completions"
        );
        let debug = format!("{endpoint:?}");
        assert!(!debug.contains("sensitive-ish"));
        assert!(debug.contains("header_count"));
    }
}
