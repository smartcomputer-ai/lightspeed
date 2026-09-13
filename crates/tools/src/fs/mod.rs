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
use thiserror::Error;
pub use vfs::{LinkedVfsFileSystem, VfsSnapshotFileSystem, VfsWorkspaceFileSystem};

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

    #[error("workspace attachment unavailable for path {path}: {message}")]
    Unavailable { path: FsPath, message: String },

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

/// Bounded recursive text search request for backends with a native search
/// implementation (e.g. a remote host that searches locally instead of
/// serving per-file reads).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsTextSearchRequest {
    pub root: FsPath,
    /// Regular expression in the Rust `regex` dialect — the same dialect the
    /// generic fallback compiles, so a pattern cannot succeed on one path and
    /// fail on the other.
    pub pattern: String,
    /// Optional glob matched against the root-relative path or file name.
    pub include: Option<String>,
    pub case_sensitive: bool,
    pub max_depth: Option<usize>,
    pub limits: FsSearchLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsSearchLimits {
    pub max_matches: u64,
    pub max_files: u64,
    pub max_bytes: u64,
    pub max_duration_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsTextSearchMatch {
    pub path: FsPath,
    pub line_number: u64,
    pub line: String,
}

/// Why a bounded search stopped before exhausting the tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsSearchStop {
    MatchLimit,
    FileLimit,
    ByteLimit,
    TimeLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsTextSearchResponse {
    pub matches: Vec<FsTextSearchMatch>,
    pub files_searched: u64,
    pub bytes_searched: u64,
    pub stopped: Option<FsSearchStop>,
}

/// Bounded recursive enumeration request for backends with a native glob
/// implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsGlobRequest {
    pub root: FsPath,
    pub pattern: String,
    pub max_depth: Option<usize>,
    pub limits: FsGlobLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsGlobLimits {
    pub max_matches: u64,
    /// Maximum directory entries visited by the traversal.
    pub max_entries: u64,
    pub max_duration_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsGlobResponse {
    pub matches: Vec<FsPath>,
    pub entries_visited: u64,
    pub stopped: Option<FsSearchStop>,
}

/// One bounded read range of a file, with the true total size so callers can
/// reject oversized files without transferring them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsRangedRead {
    pub bytes: Vec<u8>,
    pub file_size: u64,
    /// True when `bytes` is a strict prefix range of the file.
    pub truncated: bool,
}

/// Prepared workspace publication. Preparation captures the expected revision before
/// any environment bytes move; commit never merges concurrent workspace edits.
#[async_trait]
pub trait VfsCaptureTarget: Send + Sync {
    fn blob_graph(&self) -> Option<Arc<dyn engine::storage::BlobGraphStore>> {
        None
    }
    async fn commit(&self, entry: ::vfs::VfsEntry) -> FsResult<()>;
}

#[async_trait]
pub trait FileSystem: Send + Sync {
    fn access_policy(&self) -> FileAccessPolicy;

    async fn export_vfs(&self, _path: &FsPath) -> FsResult<::vfs::VfsEntry> {
        Err(FsError::Unsupported {
            message: "filesystem is not a VFS source".into(),
        })
    }
    async fn prepare_vfs_capture(
        &self,
        _path: &FsPath,
        _replace: bool,
    ) -> FsResult<Box<dyn VfsCaptureTarget>> {
        Err(FsError::Unsupported {
            message: "filesystem is not a writable VFS target".into(),
        })
    }

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

    /// Execute a bounded text search natively when the backend supports it.
    ///
    /// `Ok(None)` means the backend has no native search; callers fall back
    /// to the bounded generic traversal. Backends that do implement it must
    /// honor every limit in the request and report why they stopped.
    async fn search_text(
        &self,
        request: &FsTextSearchRequest,
    ) -> FsResult<Option<FsTextSearchResponse>> {
        let _ = request;
        Ok(None)
    }

    /// Execute a bounded recursive enumeration natively when the backend
    /// supports it; `Ok(None)` falls back to the bounded generic traversal.
    async fn glob_files(&self, request: &FsGlobRequest) -> FsResult<Option<FsGlobResponse>> {
        let _ = request;
        Ok(None)
    }

    /// Read at most `max_bytes` starting at `offset`, reporting the true file
    /// size. The default transfers the whole file and slices locally;
    /// backends with native range support truncate at the source.
    async fn read_file_range(
        &self,
        path: &FsPath,
        offset: u64,
        max_bytes: Option<u64>,
    ) -> FsResult<FsRangedRead> {
        let bytes = self.read_file(path).await?;
        Ok(ranged_read_from_full(bytes, offset, max_bytes))
    }

    fn drain_tool_effects(&self) -> Vec<ToolEffect> {
        Vec::new()
    }
}

/// Slice a fully transferred file into the requested range. Shared by the
/// trait default and backends that fall back to full transfer.
pub fn ranged_read_from_full(bytes: Vec<u8>, offset: u64, max_bytes: Option<u64>) -> FsRangedRead {
    let file_size = bytes.len() as u64;
    let start = offset.min(file_size);
    let end = max_bytes.map_or(file_size, |max| start.saturating_add(max).min(file_size));
    let slice = bytes[start as usize..end as usize].to_vec();
    let truncated = start.saturating_add(slice.len() as u64) < file_size;
    FsRangedRead {
        bytes: slice,
        file_size,
        truncated,
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
