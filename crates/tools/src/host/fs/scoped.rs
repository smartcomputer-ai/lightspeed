//! Scoped filesystem wrapper.

use std::sync::Arc;

use async_trait::async_trait;

use crate::host::fs::{
    CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
    FsPath, FsResult, ReadDirectoryEntry, RemoveOptions,
};

#[derive(Clone)]
pub struct ScopedFileSystem {
    root: FsPath,
    writable: bool,
    inner: Arc<dyn FileSystem>,
}

impl ScopedFileSystem {
    pub fn read_write(root: FsPath, inner: impl FileSystem + 'static) -> FsResult<Self> {
        Self::new(root, true, Arc::new(inner))
    }

    pub fn read_only(root: FsPath, inner: impl FileSystem + 'static) -> FsResult<Self> {
        Self::new(root, false, Arc::new(inner))
    }

    pub fn read_write_from_arc(root: FsPath, inner: Arc<dyn FileSystem>) -> FsResult<Self> {
        Self::new(root, true, inner)
    }

    pub fn read_only_from_arc(root: FsPath, inner: Arc<dyn FileSystem>) -> FsResult<Self> {
        Self::new(root, false, inner)
    }

    pub fn root(&self) -> &FsPath {
        &self.root
    }

    fn new(root: FsPath, writable: bool, inner: Arc<dyn FileSystem>) -> FsResult<Self> {
        if root.has_unresolved_parent() {
            return Err(FsError::InvalidInput {
                message: format!("scoped filesystem root cannot contain unresolved '..': {root}"),
            });
        }
        Ok(Self {
            root,
            writable,
            inner,
        })
    }

    fn resolved_path(&self, path: &FsPath) -> FsResult<FsPath> {
        if path.is_relative() && path.has_unresolved_parent() {
            return Err(FsError::PermissionDenied { path: path.clone() });
        }
        let resolved = self.root.join_segments(path.segments())?;
        if !resolved.starts_with(&self.root) {
            return Err(FsError::PermissionDenied { path: path.clone() });
        }
        Ok(resolved)
    }

    fn ensure_write_allowed(&self, original_path: &FsPath, resolved_path: &FsPath) -> FsResult<()> {
        if !self.writable || self.inner.access_policy().is_read_only() {
            return Err(FsError::PermissionDenied {
                path: original_path.clone(),
            });
        }
        if !self.inner.access_policy().can_write_path(resolved_path) {
            return Err(FsError::PermissionDenied {
                path: original_path.clone(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl FileSystem for ScopedFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        if self.writable && !self.inner.access_policy().is_read_only() {
            FileAccessPolicy::ScopedReadWrite {
                root: self.root.clone(),
            }
        } else {
            FileAccessPolicy::ScopedReadOnly {
                root: self.root.clone(),
            }
        }
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        let resolved = self.resolved_path(path)?;
        self.inner.read_file(&resolved).await
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        let resolved = self.resolved_path(path)?;
        self.ensure_write_allowed(path, &resolved)?;
        self.inner.write_file(&resolved, contents).await
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        let resolved = self.resolved_path(path)?;
        self.ensure_write_allowed(path, &resolved)?;
        self.inner.create_directory(&resolved, options).await
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let resolved = self.resolved_path(path)?;
        self.inner.get_metadata(&resolved).await
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let resolved = self.resolved_path(path)?;
        self.inner.read_directory(&resolved).await
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        let resolved = self.resolved_path(path)?;
        self.ensure_write_allowed(path, &resolved)?;
        self.inner.remove(&resolved, options).await
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        let resolved_source = self.resolved_path(source_path)?;
        let resolved_destination = self.resolved_path(destination_path)?;
        self.ensure_write_allowed(destination_path, &resolved_destination)?;
        self.inner
            .copy(&resolved_source, &resolved_destination, options)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::host::fs::{FileSystem, InMemoryFileSystem};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_file_system_resolves_paths_under_root() {
        let inner = InMemoryFileSystem::new(FileAccessPolicy::FullReadWrite);
        inner
            .create_directory(
                &FsPath::new("/workspace").unwrap(),
                CreateDirectoryOptions::single(),
            )
            .await
            .expect("create workspace");
        let fs = ScopedFileSystem::read_write(FsPath::new("/workspace").unwrap(), inner)
            .expect("scoped fs");

        fs.write_file(&FsPath::new("file.txt").unwrap(), b"hello".to_vec())
            .await
            .expect("write file");

        assert_eq!(
            fs.read_file_text(&FsPath::new("/file.txt").unwrap())
                .await
                .expect("read file"),
            "hello"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_file_system_rejects_parent_escape() {
        let inner = InMemoryFileSystem::new(FileAccessPolicy::FullReadWrite);
        let fs = ScopedFileSystem::read_write(FsPath::new("/workspace").unwrap(), inner).unwrap();

        assert!(matches!(
            fs.read_file(&FsPath::new("../secret").unwrap()).await,
            Err(FsError::PermissionDenied { .. })
        ));
    }
}
