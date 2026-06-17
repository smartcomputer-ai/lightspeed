//! Filesystem capability boundary and generic filesystem tool context.

use std::sync::Arc;

use engine::{ToolEffect, storage::BlobStore};

pub mod access;
pub mod apply_patch;
pub mod local;
pub mod memory;
pub mod path;
pub mod read_only;
pub mod scoped;
pub mod scoped_local;
pub mod session;
pub mod tools;
pub mod vfs;

pub use access::FileAccessPolicy;
use async_trait::async_trait;
pub use local::LocalFileSystem;
pub use memory::InMemoryFileSystem;
pub use path::{FsPath, FsPathError};
pub use read_only::ReadOnlyFileSystem;
pub use scoped::ScopedFileSystem;
pub use scoped_local::ScopedLocalFileSystem;
use serde::{Deserialize, Serialize};
pub use session::{
    SessionFileSystem, SessionFileSystemRoute, SessionFileSystemRouteMetadata,
    SessionFileSystemRouteSource,
};
use thiserror::Error;
pub use vfs::{MountedVfsFileSystem, VfsSnapshotFileSystem, VfsWorkspaceFileSystem};

use crate::limits::ToolLimits;

pub type FsResult<T> = Result<T, FsError>;

#[derive(Debug, Error)]
pub enum FsError {
    #[error(transparent)]
    InvalidPath(#[from] FsPathError),

    #[error("filesystem path not found: {path}")]
    NotFound { path: FsPath },

    #[error("filesystem path already exists: {path}")]
    AlreadyExists { path: FsPath },

    #[error("filesystem permission denied for path: {path}")]
    PermissionDenied { path: FsPath },

    #[error("filesystem operation unsupported: {message}")]
    Unsupported { message: String },

    #[error("invalid filesystem request: {message}")]
    InvalidInput { message: String },

    #[error("invalid filesystem data: {message}")]
    InvalidData { message: String },

    #[error("filesystem failure: {message}")]
    Failed { message: String },
}

impl FsError {
    pub fn invalid_data(error: impl std::fmt::Display) -> Self {
        Self::InvalidData {
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateDirectoryOptions {
    pub recursive: bool,
}

impl CreateDirectoryOptions {
    pub const fn recursive() -> Self {
        Self { recursive: true }
    }

    pub const fn single() -> Self {
        Self { recursive: false }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveOptions {
    pub recursive: bool,
    pub force: bool,
}

impl RemoveOptions {
    pub const fn file() -> Self {
        Self {
            recursive: false,
            force: false,
        }
    }

    pub const fn recursive() -> Self {
        Self {
            recursive: true,
            force: false,
        }
    }

    pub const fn force(mut self) -> Self {
        self.force = true;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CopyOptions {
    pub recursive: bool,
}

impl CopyOptions {
    pub const fn file() -> Self {
        Self { recursive: false }
    }

    pub const fn recursive() -> Self {
        Self { recursive: true }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileMetadata {
    pub is_directory: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub created_at_ms: i64,
    pub modified_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadDirectoryEntry {
    pub file_name: String,
    pub is_directory: bool,
    pub is_file: bool,
}

#[async_trait]
pub trait FileSystem: Send + Sync {
    fn access_policy(&self) -> FileAccessPolicy;

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>>;

    async fn read_file_text(&self, path: &FsPath) -> FsResult<String> {
        let bytes = self.read_file(path).await?;
        String::from_utf8(bytes).map_err(FsError::invalid_data)
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()>;

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()>;

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata>;

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>>;

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()>;

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()>;

    fn drain_tool_effects(&self) -> Vec<ToolEffect> {
        Vec::new()
    }
}

#[derive(Clone)]
pub struct FsToolContext {
    pub fs: Arc<dyn FileSystem>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: ToolLimits,
    pub fs_cwd: Option<FsPath>,
}

impl FsToolContext {
    pub fn new(fs: Arc<dyn FileSystem>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            fs,
            blobs,
            limits: ToolLimits::default(),
            fs_cwd: None,
        }
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_cwd(mut self, cwd: FsPath) -> Self {
        self.fs_cwd = Some(cwd);
        self
    }
}
