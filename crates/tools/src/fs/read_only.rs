//! Read-only filesystem wrapper.

use std::sync::Arc;

use async_trait::async_trait;
use harness::ToolEffect;

use crate::fs::{
    CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError, FsGlobRequest,
    FsGlobResponse, FsPath, FsRangedRead, FsResult, FsTextSearchRequest, FsTextSearchResponse,
    ReadDirectoryEntry, RemoveOptions, RenameOptions, VersionedRead, WriteOptions,
};

#[derive(Clone)]
pub struct ReadOnlyFileSystem {
    inner: Arc<dyn FileSystem>,
}

impl ReadOnlyFileSystem {
    pub fn new(inner: impl FileSystem + 'static) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn from_arc(inner: Arc<dyn FileSystem>) -> Self {
        Self { inner }
    }

    fn deny_write(&self, path: &FsPath) -> FsError {
        FsError::PermissionDenied { path: path.clone() }
    }
}

#[async_trait]
impl FileSystem for ReadOnlyFileSystem {
    async fn export_vfs(&self, path: &FsPath) -> FsResult<::vfs::VfsEntry> {
        self.inner.export_vfs(path).await
    }
    async fn prepare_vfs_capture(
        &self,
        path: &FsPath,
        _replace: bool,
    ) -> FsResult<Box<dyn crate::fs::VfsCaptureTarget>> {
        Err(self.deny_write(path))
    }
    fn access_policy(&self) -> FileAccessPolicy {
        match self.inner.access_policy() {
            FileAccessPolicy::FullReadWrite | FileAccessPolicy::FullReadOnly => {
                FileAccessPolicy::FullReadOnly
            }
            FileAccessPolicy::ScopedReadWrite { root }
            | FileAccessPolicy::ScopedReadOnly { root } => {
                FileAccessPolicy::ScopedReadOnly { root }
            }
        }
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        self.inner.read_file(path).await
    }

    async fn write_file(&self, path: &FsPath, _contents: Vec<u8>) -> FsResult<()> {
        Err(self.deny_write(path))
    }

    async fn write_file_ref(
        &self,
        path: &FsPath,
        _content: &crate::content::ContentDescriptor,
    ) -> FsResult<()> {
        Err(self.deny_write(path))
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        _options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        Err(self.deny_write(path))
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        self.inner.get_metadata(path).await
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        self.inner.read_directory(path).await
    }

    async fn remove(&self, path: &FsPath, _options: RemoveOptions) -> FsResult<()> {
        Err(self.deny_write(path))
    }

    async fn rename(
        &self,
        source_path: &FsPath,
        _destination_path: &FsPath,
        _options: RenameOptions,
    ) -> FsResult<()> {
        Err(self.deny_write(source_path))
    }

    async fn read_file_versioned(&self, path: &FsPath) -> FsResult<VersionedRead> {
        self.inner.read_file_versioned(path).await
    }

    async fn write_file_with(
        &self,
        path: &FsPath,
        _contents: Vec<u8>,
        _options: WriteOptions,
    ) -> FsResult<()> {
        Err(self.deny_write(path))
    }

    async fn search_text(
        &self,
        request: &FsTextSearchRequest,
    ) -> FsResult<Option<FsTextSearchResponse>> {
        self.inner.search_text(request).await
    }

    async fn glob_files(&self, request: &FsGlobRequest) -> FsResult<Option<FsGlobResponse>> {
        self.inner.glob_files(request).await
    }

    fn max_read_bytes(&self) -> Option<u64> {
        self.inner.max_read_bytes()
    }

    async fn read_file_range(
        &self,
        path: &FsPath,
        offset: u64,
        max_bytes: Option<u64>,
    ) -> FsResult<FsRangedRead> {
        self.inner.read_file_range(path, offset, max_bytes).await
    }

    fn drain_tool_effects(&self) -> Vec<ToolEffect> {
        self.inner.drain_tool_effects()
    }
}

#[cfg(test)]
mod tests {
    use crate::fs::{FileSystem, InMemoryFileSystem};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_wrapper_rejects_writes() {
        let inner = InMemoryFileSystem::new(FileAccessPolicy::FullReadWrite);
        inner
            .write_file(&FsPath::new("/file.txt").unwrap(), b"hello".to_vec())
            .await
            .expect("seed file");
        let fs = ReadOnlyFileSystem::new(inner);

        assert_eq!(
            fs.read_file_text(&FsPath::new("/file.txt").unwrap())
                .await
                .expect("read file"),
            "hello"
        );
        assert!(matches!(
            fs.write_file(&FsPath::new("/file.txt").unwrap(), b"updated".to_vec())
                .await,
            Err(FsError::PermissionDenied { .. })
        ));
    }
}
