//! Filesystem capability boundary and generic filesystem tool context.

use std::sync::Arc;

use harness::{ToolEffect, storage::BlobStore};

pub mod access;
pub mod apply_patch;
pub mod local;
pub mod memory;
pub mod path;
pub mod pattern;
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
pub use vfs::{AttachedVfsFileSystem, VfsSnapshotFileSystem, VfsWorkspaceFileSystem};

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

    #[error("file changed since it was read: {path}; read it again before writing")]
    Conflict { path: FsPath },

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenameOptions {
    /// Replace an existing destination file; otherwise an existing
    /// destination is [`FsError::AlreadyExists`].
    pub overwrite: bool,
}

/// Opaque token naming one version of a file's content, as the backend
/// reported it on a read. Equal tokens for one path mean unchanged content.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct FsVersion(String);

impl FsVersion {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A whole-file read together with the version it observed, when the
/// backend tracks versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedRead {
    pub bytes: Vec<u8>,
    pub version: Option<FsVersion>,
}

/// Preconditions of a write.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WriteOptions {
    /// Write only while the file still has this version; otherwise the write
    /// fails with [`FsError::Conflict`]. Only versions the same backend
    /// returned from [`FileSystem::read_file_versioned`] are meaningful.
    pub if_match: Option<FsVersion>,
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
    /// Optional path pattern in the [`pattern`] grammar selecting the files
    /// searched.
    pub include: Option<String>,
    pub case_sensitive: bool,
    /// Deepest entry depth searched; the root's direct children are depth 1.
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
    /// The line was cut at the backend's line length bound.
    pub line_truncated: bool,
}

/// Why a bounded search stopped before exhausting the tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsSearchStop {
    MatchLimit,
    FileLimit,
    ByteLimit,
    TimeLimit,
    /// The matches filled the backend's response budget.
    ResponseLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsTextSearchResponse {
    pub matches: Vec<FsTextSearchMatch>,
    pub files_searched: u64,
    pub bytes_searched: u64,
    /// Files left out because they exceeded the remaining byte budget, were
    /// binary or could not be read; none of them ends the search.
    pub skipped_files: u64,
    pub stopped: Option<FsSearchStop>,
}

/// Bounded recursive enumeration request for backends with a native glob
/// implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsGlobRequest {
    pub root: FsPath,
    /// A path pattern in the [`pattern`] grammar.
    pub pattern: String,
    /// Deepest entry depth visited; the root's direct children are depth 1.
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
    /// Bytes exist after `offset` plus the length of `bytes`.
    pub truncated: bool,
}

/// Prepared workspace publication. Preparation captures the expected revision before
/// any environment bytes move; commit never merges concurrent workspace edits.
#[async_trait]
pub trait VfsCaptureTarget: Send + Sync {
    fn blob_graph(&self) -> Option<Arc<dyn harness::storage::BlobGraphStore>> {
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

    /// Read a whole file together with its version. Backends without
    /// versions report none, and their writes need no precondition.
    async fn read_file_versioned(&self, path: &FsPath) -> FsResult<VersionedRead> {
        Ok(VersionedRead {
            bytes: self.read_file(path).await?,
            version: None,
        })
    }

    /// Write a whole file under `options`. Backends without versions never
    /// hand out a version, so the default is only reached without one.
    async fn write_file_with(
        &self,
        path: &FsPath,
        contents: Vec<u8>,
        options: WriteOptions,
    ) -> FsResult<()> {
        if options.if_match.is_some() {
            return Err(FsError::Unsupported {
                message: "filesystem does not track file versions".into(),
            });
        }
        self.write_file(path, contents).await
    }

    /// Link existing immutable content without loading its bytes into the tool worker.
    async fn write_file_ref(
        &self,
        _path: &FsPath,
        _content: &crate::content::ContentDescriptor,
    ) -> FsResult<()> {
        Err(FsError::Unsupported {
            message: "filesystem does not support writing content references".into(),
        })
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()>;

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata>;

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>>;

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()>;

    /// Move a file to `destination_path`, keeping its mode. The default
    /// writes the destination and removes the source; backends with a
    /// native rename override it to move atomically.
    async fn rename(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: RenameOptions,
    ) -> FsResult<()> {
        let metadata = self.get_metadata(source_path).await?;
        if !metadata.is_file {
            return Err(FsError::Unsupported {
                message: format!("{source_path} is not a file; only files can be moved"),
            });
        }
        if !options.overwrite {
            match self.get_metadata(destination_path).await {
                Ok(_) => {
                    return Err(FsError::AlreadyExists {
                        path: destination_path.clone(),
                    });
                }
                Err(FsError::NotFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        let contents = self.read_file(source_path).await?;
        self.write_file(destination_path, contents).await?;
        self.remove(source_path, RemoveOptions::file()).await
    }

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

    /// Largest range one [`read_file_range`](Self::read_file_range) returns,
    /// when the backend bounds it; longer requests come back `truncated`.
    fn max_read_bytes(&self) -> Option<u64> {
        None
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
    pub content_resolver: crate::content::ContentResolver,
}

impl FsToolContext {
    pub fn new(fs: Arc<dyn FileSystem>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            fs,
            content_resolver: crate::content::ContentResolver::new(blobs.clone()),
            blobs,
            limits: ToolLimits::default(),
            fs_cwd: None,
        }
    }

    pub fn with_content_resolver(mut self, resolver: crate::content::ContentResolver) -> Self {
        self.content_resolver = resolver;
        self
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
