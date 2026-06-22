//! Read-only filesystem wrapper.

use std::sync::Arc;

use async_trait::async_trait;
use engine::ToolEffect;

use crate::fs::{
    CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
    FsPath, FsResult, ReadDirectoryEntry, RemoveOptions,
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

    async fn copy(
        &self,
        _source_path: &FsPath,
        destination_path: &FsPath,
        _options: CopyOptions,
    ) -> FsResult<()> {
        Err(self.deny_write(destination_path))
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
