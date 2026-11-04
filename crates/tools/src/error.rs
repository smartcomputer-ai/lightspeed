use thiserror::Error;

use harness::storage::BlobStoreError;

use crate::{
    environment::jobs::JobError,
    environment::process::ProcessError,
    fs::{
        FsError, FsPath,
        apply_patch::{ApplyPatchSummary, engine::partially_applied_message},
    },
};

pub type ToolResult<T> = Result<T, ToolError>;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error(transparent)]
    Content(#[from] crate::content::ContentError),

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

    /// A patch stopped partway through its writes. The model has to hear what
    /// landed, because resending those hunks could apply them twice.
    #[error(
        "{}",
        partially_applied_message(applied, failed_path, not_applied, source)
    )]
    PatchPartiallyApplied {
        /// Boxed so every `ToolResult` stays small.
        applied: Box<ApplyPatchSummary>,
        failed_path: FsPath,
        not_applied: Vec<String>,
        source: FsError,
    },
}
