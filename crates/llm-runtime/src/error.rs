use engine::{BlobRef, ContextEntryId, ProviderApiKind};
use thiserror::Error;

pub type LlmAdapterResult<T> = Result<T, LlmAdapterError>;

#[derive(Debug, Error)]
pub enum LlmAdapterError {
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

    #[error("provider call failed: {message}")]
    Provider { message: String },
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
            message: error.to_string(),
        }
    }
}
