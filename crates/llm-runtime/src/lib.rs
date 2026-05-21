//! LLM runtime adapters for Forge-native agent sessions.
//!
//! This crate connects `agent-core` LLM request records to provider-native
//! `llm-clients` clients without making the deterministic agent core depend on
//! provider clients or HTTP configuration.

pub mod anthropic_messages;
pub mod blob_io;
pub mod error;
pub mod executor;
pub mod openai_completions;
pub mod openai_responses;
pub mod result;
pub mod testing;

pub use error::{LlmAdapterError, LlmAdapterResult};
pub use executor::{LlmAdapterRegistry, LlmGenerationAdapter, LlmRuntime};
pub use openai_responses::{OpenAiResponsesApi, OpenAiResponsesLlmAdapter};
pub use result::{LlmGenerationExecution, failed_generation_result};
