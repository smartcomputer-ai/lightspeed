//! HTTP client wrapper and URL helpers for provider clients.

use crate::error::{ConfigurationError, LlmApiError, TransportError};
use crate::transport::HeaderSnapshot;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, RequestBuilder, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HttpClientConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(120),
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

    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.default_headers.insert(name, value);
        self
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
}
