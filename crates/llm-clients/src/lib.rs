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
