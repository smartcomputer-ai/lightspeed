use thiserror::Error;

use agent_core::storage::BlobStoreError;

use crate::host::{fs::FsError, process::ProcessError};

pub type ToolResult<T> = Result<T, ToolError>;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error(transparent)]
    Filesystem(#[from] FsError),

    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("unsupported tool capability: {message}")]
    UnsupportedCapability { message: String },

    #[error("invalid tool request: {message}")]
    InvalidRequest { message: String },
}
