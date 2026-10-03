use thiserror::Error;

use harness::storage::BlobStoreError;

use crate::{environment::jobs::JobError, environment::process::ProcessError, fs::FsError};

pub type ToolResult<T> = Result<T, ToolError>;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error(transparent)]
    Filesystem(#[from] FsError),

    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error(transparent)]
    Job(#[from] JobError),

    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("unsupported tool capability: {message}")]
    UnsupportedCapability { message: String },

    #[error("invalid tool request: {message}")]
    InvalidRequest { message: String },
}
