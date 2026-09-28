//! LLM runtime adapters for Lightspeed-native agent sessions.
//!
//! This crate connects `engine` LLM request records to provider-native
//! `llm-clients` clients without making the deterministic agent core depend on
//! provider clients or HTTP configuration.

pub mod anthropic_messages;
pub mod blob_io;
mod catalog_prompts;
pub mod error;
pub mod executor;
pub mod mcp;
pub mod openai_completions;
pub mod openai_responses;
pub mod params;
mod prompt_cache;
pub mod provider_keys;
pub mod result;
pub mod secrets;
pub mod testing;

pub use anthropic_messages::{
    ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND, AnthropicMessagesApi,
    AnthropicMessagesLlmAdapter,
};
pub use error::{LlmAdapterError, LlmAdapterResult};
pub use executor::{LlmAdapterRegistry, LlmCompactionAdapter, LlmGenerationAdapter, LlmRuntime};
pub use mcp::{McpInventoryError, McpInventoryResolver, NativeMcpTool};
pub use openai_completions::{OpenAiCompletionsApi, OpenAiCompletionsLlmAdapter};
pub use openai_responses::{OpenAiResponsesApi, OpenAiResponsesLlmAdapter};
pub use params::{
    AnthropicMessagesParams, AnthropicThinkingConfig, OpenAiCompletionsParams,
    OpenAiReasoningConfig, OpenAiResponsesParams, OpenAiServiceTier, PROVIDER_PARAMS_VERSION,
    validate_provider_params,
};
pub use provider_keys::{
    ModelProviderResolver, NoStoredModelProviders, NoStoredProviderKeys, ProviderAuthScheme,
    ProviderKeyError, ResolvedEndpoint, ResolvedModelProvider, ResolvedProviderAuth,
    StaticModelProviders, StaticProviderKeys, resolve_provider_route,
};
pub use result::{LlmDebugDumps, LlmGenerationExecution, failed_generation_result};
pub use secrets::{
    EnvSecretResolver, REDACTED_SECRET_PLACEHOLDER, ResolvedSecretValue, SECRET_NAMESPACE_ENV,
    SECRET_NAMESPACE_MCP_SERVER, SecretResolveError, SecretResolver, StaticSecretResolver,
    UnconfiguredSecretResolver,
};
mod tool_catalog;
