//! LLM runtime adapters for Lightspeed-native agent sessions.
//!
//! This crate connects `engine` LLM request records to provider-native
//! `llm-clients` clients without making the deterministic agent core depend on
//! provider clients or HTTP configuration.

pub mod anthropic_messages;
pub mod blob_io;
pub mod error;
pub mod executor;
pub mod openai_completions;
pub mod openai_responses;
pub mod params;
pub mod provider_keys;
pub mod result;
pub mod secrets;
mod skill_prompts;
pub mod testing;

pub use anthropic_messages::{
    ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND, AnthropicMessagesApi,
    AnthropicMessagesLlmAdapter,
};
pub use error::{LlmAdapterError, LlmAdapterResult};
pub use executor::{LlmAdapterRegistry, LlmCompactionAdapter, LlmGenerationAdapter, LlmRuntime};
pub use openai_responses::{OpenAiResponsesApi, OpenAiResponsesLlmAdapter};
pub use provider_keys::{
    NoStoredProviderKeys, ProviderAuthScheme, ProviderKeyError, ProviderKeyResolver,
    ResolvedProviderAuth, StaticProviderKeys,
};
pub use secrets::{
    EnvSecretResolver, REDACTED_SECRET_PLACEHOLDER, ResolvedSecretValue, SECRET_NAMESPACE_AUTH_GRANT,
    SECRET_NAMESPACE_ENV, SecretResolveError, SecretResolver, StaticSecretResolver,
    UnconfiguredSecretResolver,
};
pub use params::{
    AnthropicMessagesParams, AnthropicThinkingConfig, OpenAiCompletionsParams,
    OpenAiReasoningConfig, OpenAiResponsesParams, PROVIDER_PARAMS_VERSION,
    validate_provider_params,
};
pub use result::{LlmGenerationExecution, failed_generation_result};
