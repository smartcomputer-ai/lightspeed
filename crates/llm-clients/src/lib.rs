//! Provider-native LLM API client primitives for Lightspeed.
//!
//! `llm-clients` is intentionally lower-level than the Lightspeed agent. It owns
//! provider HTTP transport, native API request/response modules, stream parsing,
//! and provider error classification. It does not define sessions, tools,
//! context windows, CAS refs, or a provider-neutral model message abstraction.

pub mod anthropic;
pub mod content;
pub mod error;
pub mod openai;
pub mod transport;

pub use error::{
    ConfigurationError, DecodeError, LlmApiError, ProviderFailureKind, ProviderHttpError,
    StreamError, TransportError, UnsupportedOperation,
};
pub use transport::{
    ApiResponse, ApiStreamEvent, EndpointOverride, HeaderSnapshot, HttpClient, SseEvent, SseParser,
};

/// Per-request authentication override for provider clients. Overrides the
/// client's transport-configured key for one request; the scheme decides how
/// the credential is sent (provider-native key header vs OAuth bearer).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequestAuth<'a> {
    /// Explicitly send no authentication. This differs from an absent
    /// per-request override, which falls back to the client's configured key.
    None,
    /// Provider API key, sent in the provider's native key header
    /// (`x-api-key` for Anthropic, `Authorization: Bearer` for OpenAI).
    ApiKey(&'a str),
    /// OAuth access token, sent as `Authorization: Bearer` (Anthropic also
    /// requires its OAuth beta header).
    Bearer(&'a str),
    /// A broker-issued bearer token restricted to its approved resource URL.
    ScopedBearer(ScopedBearer<'a>),
}

impl std::fmt::Debug for RequestAuth<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("RequestAuth::None"),
            Self::ApiKey(_) => f.write_str("RequestAuth::ApiKey(<redacted>)"),
            Self::Bearer(_) | Self::ScopedBearer(_) => {
                f.write_str("RequestAuth::Bearer(<redacted>)")
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ScopedBearer<'a> {
    token: &'a str,
    audience: &'a str,
}

impl<'a> ScopedBearer<'a> {
    pub fn new(token: &'a str, audience: &'a str) -> Self {
        Self { token, audience }
    }

    /// Compare the broker-approved audience with the normalized base the
    /// client actually sends to, including deployment endpoint overrides.
    fn token_for(self, base_url: &reqwest::Url) -> Result<&'a str, LlmApiError> {
        // Joining a provider API path discards any query or fragment on the
        // deployment's base URL.
        let base = base_url.as_str();
        let target = base.split_once(['?', '#']).map_or(base, |(path, _)| path);
        if target == self.audience
            || target
                .strip_prefix(self.audience)
                .is_some_and(|rest| self.audience.ends_with('/') || rest.starts_with('/'))
        {
            return Ok(self.token);
        }
        Err(
            ConfigurationError::new("model OAuth audience does not cover the provider endpoint")
                .into(),
        )
    }
}
