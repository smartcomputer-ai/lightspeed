use engine::{BlobRef, ContextEntryId, ProviderApiKind};
use thiserror::Error;

pub type LlmAdapterResult<T> = Result<T, LlmAdapterError>;

#[derive(Debug, Error)]
pub enum LlmAdapterError {
    #[error("context limit exceeded: {message}")]
    ContextLimit { message: String },
    #[error("unsupported LLM provider API kind: {api_kind:?}")]
    UnsupportedApiKind { api_kind: ProviderApiKind },

    #[error("LLM request kind does not match provider API kind: {message}")]
    RequestKindMismatch { message: String },

    #[error("missing context entry {entry_id}")]
    MissingContextEntry { entry_id: ContextEntryId },

    #[error("blob store failure: {message}")]
    BlobStore { message: String },

    #[error("blob {blob_ref} is not valid UTF-8: {message}")]
    InvalidUtf8 { blob_ref: BlobRef, message: String },

    #[error("invalid JSON in blob {blob_ref}: {message}")]
    InvalidJson { blob_ref: BlobRef, message: String },

    #[error("invalid provider request: {message}")]
    InvalidProviderRequest { message: String },

    #[error("model {model} does not support {feature}: {message}")]
    UnsupportedModelFeature {
        model: String,
        feature: &'static str,
        message: String,
    },

    #[error("failed to resolve auth secret for tool {tool}: {message}")]
    SecretResolution { tool: String, message: String },

    #[error("failed to resolve native MCP inventory for {server}: {message}")]
    McpInventory { server: String, message: String },

    #[error("failed to resolve provider API key: {message}")]
    ProviderKeyResolution { message: String },

    /// A provider client call failed. The typed client error is retained so
    /// the runtime can preserve its retry disposition instead of flattening
    /// classification into a string.
    #[error("provider call failed: {source}")]
    Provider {
        #[source]
        source: Box<llm_clients::LlmApiError>,
    },
}

impl From<engine::storage::BlobStoreError> for LlmAdapterError {
    fn from(error: engine::storage::BlobStoreError) -> Self {
        Self::BlobStore {
            message: error.to_string(),
        }
    }
}

impl From<llm_clients::LlmApiError> for LlmAdapterError {
    fn from(error: llm_clients::LlmApiError) -> Self {
        Self::Provider {
            source: Box::new(error),
        }
    }
}
