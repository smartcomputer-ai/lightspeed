//! Provider-native LLM API client primitives for Forge.
//!
//! `llm-clients` is intentionally lower-level than the Forge agent. It owns
//! provider HTTP transport, native API request/response modules, stream parsing,
//! and provider error classification. It does not define sessions, tools,
//! context windows, CAS refs, or a provider-neutral model message abstraction.

pub mod anthropic;
pub mod error;
pub mod openai;
pub mod transport;

pub use error::{
    ConfigurationError, DecodeError, LlmApiError, ProviderFailureKind, ProviderHttpError,
    StreamError, TransportError, UnsupportedOperation,
};
pub use transport::{ApiResponse, ApiStreamEvent, HeaderSnapshot, HttpClient, SseEvent, SseParser};

/// Per-request authentication override for provider clients. Overrides the
/// client's transport-configured key for one request; the scheme decides how
/// the credential is sent (provider-native key header vs OAuth bearer).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequestAuth<'a> {
    /// Provider API key, sent in the provider's native key header
    /// (`x-api-key` for Anthropic, `Authorization: Bearer` for OpenAI).
    ApiKey(&'a str),
    /// OAuth access token, sent as `Authorization: Bearer` (Anthropic also
    /// requires its OAuth beta header).
    Bearer(&'a str),
}

impl std::fmt::Debug for RequestAuth<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("RequestAuth::ApiKey(<redacted>)"),
            Self::Bearer(_) => f.write_str("RequestAuth::Bearer(<redacted>)"),
        }
    }
}
