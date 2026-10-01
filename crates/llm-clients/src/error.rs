//! API-client error types shared by provider-native modules.

use crate::transport::HeaderSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("configuration error: {message}")]
pub struct ConfigurationError {
    pub message: String,
}

impl ConfigurationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("transport error: {message}")]
pub struct TransportError {
    pub message: String,
    pub retryable: bool,
}

impl TransportError {
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("decode error: {message}")]
pub struct DecodeError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl DecodeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            raw: None,
        }
    }

    pub fn with_raw(message: impl Into<String>, raw: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            raw: Some(raw.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("stream error: {message}")]
pub struct StreamError {
    pub message: String,
    pub retryable: bool,
}

impl StreamError {
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("unsupported operation for {api_kind}: {operation}")]
pub struct UnsupportedOperation {
    pub api_kind: String,
    pub operation: String,
}

impl UnsupportedOperation {
    pub fn new(api_kind: impl Into<String>, operation: impl Into<String>) -> Self {
        Self {
            api_kind: api_kind.into(),
            operation: operation.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureKind {
    Authentication,
    AccessDenied,
    NotFound,
    InvalidRequest,
    RateLimit,
    Server,
    ContentFilter,
    ContextLength,
    QuotaExceeded,
    Timeout,
    Network,
    Other,
}

impl ProviderFailureKind {
    /// Whether the same provider call may be retried by default. Terminal is
    /// the safe default: `Other` carries no transient evidence, so it must
    /// not inherit a retry budget merely because its kind is unknown.
    pub fn default_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::Server | Self::Timeout | Self::Network
        )
    }

    /// Whether the provider refused the request itself, as invalid or larger
    /// than the model accepts, rather than failing to serve it.
    pub fn rejects_request(self) -> bool {
        matches!(self, Self::InvalidRequest | Self::ContextLength)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Error)]
#[error("{api_kind} provider HTTP error {status}: {message}")]
pub struct ProviderHttpError {
    pub api_kind: String,
    pub status: u16,
    pub kind: ProviderFailureKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none", with = "duration_secs")]
    pub retry_after: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    pub headers: HeaderSnapshot,
}

impl ProviderHttpError {
    pub fn new(
        api_kind: impl Into<String>,
        status: reqwest::StatusCode,
        message: impl Into<String>,
        headers: HeaderSnapshot,
    ) -> Self {
        let kind = classify_status(status.as_u16(), None, None, None);
        Self {
            api_kind: api_kind.into(),
            status: status.as_u16(),
            kind,
            message: message.into(),
            error_code: None,
            error_type: None,
            retryable: kind.default_retryable(),
            retry_after: headers.retry_after(),
            raw_json: None,
            raw_text: None,
            headers,
        }
    }

    pub fn with_provider_details(
        mut self,
        error_code: Option<String>,
        error_type: Option<String>,
        provider_message: Option<String>,
        raw_json: Option<Value>,
        raw_text: Option<String>,
    ) -> Self {
        self.kind = classify_status(
            self.status,
            error_code.as_deref(),
            error_type.as_deref(),
            provider_message.as_deref(),
        );
        self.retryable = self.kind.default_retryable();
        if let Some(message) = provider_message {
            self.message = message;
        }
        self.error_code = error_code;
        self.error_type = error_type;
        self.raw_json = raw_json;
        self.raw_text = raw_text;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Error)]
pub enum LlmApiError {
    #[error(transparent)]
    Configuration(#[from] ConfigurationError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    HttpStatus(Box<ProviderHttpError>),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error(transparent)]
    Unsupported(#[from] UnsupportedOperation),
}

impl From<ProviderHttpError> for LlmApiError {
    fn from(error: ProviderHttpError) -> Self {
        Self::HttpStatus(Box::new(error))
    }
}

impl LlmApiError {
    /// Whether the exact same provider call may be retried. Configuration,
    /// decode, and unsupported-operation errors are terminal; transport,
    /// stream, and HTTP failures carry their own classification.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport(error) => error.retryable,
            Self::HttpStatus(error) => error.retryable,
            Self::Stream(error) => error.retryable,
            Self::Configuration(_) | Self::Decode(_) | Self::Unsupported(_) => false,
        }
    }

    /// The provider's HTTP error when it rejected the request itself.
    pub fn request_rejection(&self) -> Option<&ProviderHttpError> {
        match self {
            Self::HttpStatus(error) if error.kind.rejects_request() => Some(error),
            _ => None,
        }
    }

    /// Provider-suggested delay before retrying, when advertised.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::HttpStatus(error) => error.retry_after,
            _ => None,
        }
    }
}

pub fn classify_status(
    status: u16,
    error_code: Option<&str>,
    error_type: Option<&str>,
    message: Option<&str>,
) -> ProviderFailureKind {
    let haystack = [error_code, error_type, message]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    if haystack.contains("context_length")
        || haystack.contains("context length")
        || haystack.contains("maximum context")
        || haystack.contains("too many tokens")
    {
        return ProviderFailureKind::ContextLength;
    }
    if haystack.contains("content_filter")
        || haystack.contains("content filter")
        || haystack.contains("safety")
    {
        return ProviderFailureKind::ContentFilter;
    }
    if haystack.contains("quota") || haystack.contains("insufficient_quota") {
        return ProviderFailureKind::QuotaExceeded;
    }

    match status {
        401 => ProviderFailureKind::Authentication,
        403 => ProviderFailureKind::AccessDenied,
        404 => ProviderFailureKind::NotFound,
        408 => ProviderFailureKind::Timeout,
        400 | 422 => ProviderFailureKind::InvalidRequest,
        429 => ProviderFailureKind::RateLimit,
        500..=599 => ProviderFailureKind::Server,
        _ => ProviderFailureKind::Other,
    }
}

mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(duration) => serializer.serialize_some(&duration.as_secs_f64()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<f64>::deserialize(deserializer)?;
        Ok(value.map(Duration::from_secs_f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_status_uses_http_status_when_payload_has_no_hint() {
        assert_eq!(
            classify_status(401, None, None, None),
            ProviderFailureKind::Authentication
        );
        assert_eq!(
            classify_status(429, None, None, None),
            ProviderFailureKind::RateLimit
        );
        assert_eq!(
            classify_status(503, None, None, None),
            ProviderFailureKind::Server
        );
    }

    #[test]
    fn classify_status_uses_provider_context_length_hint() {
        assert_eq!(
            classify_status(400, Some("context_length_exceeded"), None, None),
            ProviderFailureKind::ContextLength
        );
        assert_eq!(
            classify_status(400, None, None, Some("too many tokens")),
            ProviderFailureKind::ContextLength
        );
    }

    #[test]
    fn classify_status_uses_provider_content_filter_hint() {
        assert_eq!(
            classify_status(400, Some("content_filter"), None, None),
            ProviderFailureKind::ContentFilter
        );
    }

    #[test]
    fn temporary_rate_limit_is_retryable_but_quota_429_is_terminal() {
        let temporary = classify_status(429, Some("rate_limit_exceeded"), None, None);
        assert_eq!(temporary, ProviderFailureKind::RateLimit);
        assert!(temporary.default_retryable());

        let quota = classify_status(429, Some("insufficient_quota"), None, None);
        assert_eq!(quota, ProviderFailureKind::QuotaExceeded);
        assert!(!quota.default_retryable());
    }

    #[test]
    fn unknown_failure_kind_is_terminal_by_default() {
        assert!(!ProviderFailureKind::Other.default_retryable());
        assert!(!classify_status(302, None, None, None).default_retryable());
    }

    #[test]
    fn retry_disposition_follows_variant_classification() {
        let overloaded = LlmApiError::from(
            ProviderHttpError::new(
                "openai:responses",
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                "Our servers are currently overloaded.",
                HeaderSnapshot::default(),
            )
            .with_provider_details(None, None, None, None, None),
        );
        assert!(overloaded.retryable());

        let auth = LlmApiError::from(ProviderHttpError::new(
            "openai:responses",
            reqwest::StatusCode::UNAUTHORIZED,
            "bad key",
            HeaderSnapshot::default(),
        ));
        assert!(!auth.retryable());
        assert_eq!(auth.retry_after(), None);

        assert!(LlmApiError::from(TransportError::new("connect reset", true)).retryable());
        assert!(!LlmApiError::from(TransportError::new("tls misconfigured", false)).retryable());
        assert!(LlmApiError::from(StreamError::new("stream cut", true)).retryable());
        assert!(!LlmApiError::from(ConfigurationError::new("no base url")).retryable());
        assert!(!LlmApiError::from(DecodeError::new("bad json")).retryable());
        assert!(
            !LlmApiError::from(UnsupportedOperation::new("openai:responses", "images")).retryable()
        );
    }

    #[test]
    fn retry_after_is_surfaced_only_from_http_failures() {
        let mut error = ProviderHttpError::new(
            "anthropic:messages",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "slow down",
            HeaderSnapshot::default(),
        );
        error.retry_after = Some(Duration::from_secs(7));
        let error = LlmApiError::from(error);
        assert!(error.retryable());
        assert_eq!(error.retry_after(), Some(Duration::from_secs(7)));
    }
}
