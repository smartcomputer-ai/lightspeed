//! Read-only filesystem adapter over a CAS-backed VFS snapshot.

use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    BlobRef,
    storage::{BlobStore, BlobStoreError},
};

use crate::host::fs::{
    CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
    FsPath, FsResult, ReadDirectoryEntry, RemoveOptions,
};

#[derive(Clone)]
pub struct VfsSnapshotFileSystem {
    blobs: Arc<dyn BlobStore>,
    snapshot_ref: BlobRef,
    manifest: Arc<::vfs::VfsSnapshotManifest>,
}

impl VfsSnapshotFileSystem {
    pub async fn new(
        blobs: Arc<dyn BlobStore>,
        snapshot_ref: BlobRef,
    ) -> Result<Self, ::vfs::VfsError> {
        let manifest = ::vfs::read_snapshot_manifest(blobs.as_ref(), &snapshot_ref).await?;
        Ok(Self::from_manifest(blobs, snapshot_ref, manifest))
    }

    pub fn from_manifest(
        blobs: Arc<dyn BlobStore>,
        snapshot_ref: BlobRef,
        manifest: ::vfs::VfsSnapshotManifest,
    ) -> Self {
        Self {
            blobs,
            snapshot_ref,
            manifest: Arc::new(manifest),
        }
    }

    pub fn snapshot_ref(&self) -> &BlobRef {
        &self.snapshot_ref
    }

    fn vfs_path(&self, path: &FsPath) -> FsResult<::vfs::VfsPath> {
        if path.has_unresolved_parent() {
            return Err(FsError::InvalidInput {
                message: format!("vfs path cannot contain unresolved parent components: {path}"),
            });
        }
        if path.is_root() {
            return Ok(::vfs::VfsPath::root());
        }
        ::vfs::VfsPath::parse(path.as_str()).map_err(|error| FsError::InvalidInput {
            message: error.to_string(),
        })
    }

    fn deny_write(&self, path: &FsPath) -> FsError {
        FsError::PermissionDenied { path: path.clone() }
    }
}

#[async_trait]
impl FileSystem for VfsSnapshotFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        FileAccessPolicy::FullReadOnly
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        let vfs_path = self.vfs_path(path)?;
        ::vfs::read_snapshot_file(self.blobs.as_ref(), &self.manifest, &vfs_path)
            .await
            .map_err(|error| map_vfs_error(error, path))
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
        let vfs_path = self.vfs_path(path)?;
        let metadata = ::vfs::stat_snapshot_path(&self.manifest, &vfs_path)
            .map_err(|error| map_vfs_error(error, path))?;
        Ok(FileMetadata {
            is_directory: metadata.is_directory,
            is_file: metadata.is_file,
            is_symlink: false,
            created_at_ms: 0,
            modified_at_ms: 0,
        })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let vfs_path = self.vfs_path(path)?;
        ::vfs::list_snapshot_directory(&self.manifest, &vfs_path)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| ReadDirectoryEntry {
                        file_name: entry.file_name,
                        is_directory: entry.is_directory,
                        is_file: entry.is_file,
                    })
                    .collect()
            })
            .map_err(|error| map_vfs_error(error, path))
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
}

fn map_vfs_error(error: ::vfs::VfsError, request_path: &FsPath) -> FsError {
    match error {
        ::vfs::VfsError::NotFound { .. } => FsError::NotFound {
            path: request_path.clone(),
        },
        ::vfs::VfsError::NotAFile { path } => FsError::InvalidInput {
            message: format!("path is not a file: {path}"),
        },
        ::vfs::VfsError::NotADirectory { path } => FsError::InvalidInput {
            message: format!("path is not a directory: {path}"),
        },
        ::vfs::VfsError::BlobStore(BlobStoreError::NotFound { blob_ref }) => FsError::InvalidData {
            message: format!("vfs snapshot references missing blob: {blob_ref}"),
        },
        ::vfs::VfsError::BlobStore(error) => FsError::Failed {
            message: error.to_string(),
        },
        ::vfs::VfsError::InvalidPath(error) => FsError::InvalidInput {
            message: error.to_string(),
        },
        ::vfs::VfsError::InvalidManifest { message } => FsError::InvalidData { message },
        error => FsError::Failed {
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use engine::storage::InMemoryBlobStore;

    use super::*;

    async fn test_fs() -> VfsSnapshotFileSystem {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let result = ::vfs::create_inline_snapshot(
            blobs.as_ref(),
            ::vfs::CreateInlineSnapshotRequest::new(vec![
                ::vfs::InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
                ::vfs::InlineFile::new("src/lib.rs", b"pub fn f() {}\n".to_vec()).unwrap(),
            ]),
        )
        .await
        .expect("create snapshot");

        VfsSnapshotFileSystem::from_manifest(blobs, result.snapshot_ref, result.manifest)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_file_system_reads_files() {
        let fs = test_fs().await;

        assert_eq!(
            fs.read_file_text(&FsPath::new("/README.md").unwrap())
                .await
                .expect("read absolute file"),
            "hello\n"
        );
        assert_eq!(
            fs.read_file_text(&FsPath::new("src/lib.rs").unwrap())
                .await
                .expect("read relative file"),
            "pub fn f() {}\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_file_system_lists_directories() {
        let fs = test_fs().await;

        let root = fs.read_directory(&FsPath::root()).await.expect("list root");
        assert_eq!(
            root,
            vec![
                ReadDirectoryEntry {
                    file_name: "README.md".to_string(),
                    is_directory: false,
                    is_file: true,
                },
                ReadDirectoryEntry {
                    file_name: "src".to_string(),
                    is_directory: true,
                    is_file: false,
                },
            ]
        );

        let src = fs
            .read_directory(&FsPath::new("/src").unwrap())
            .await
            .expect("list src");
        assert_eq!(
            src,
            vec![ReadDirectoryEntry {
                file_name: "lib.rs".to_string(),
                is_directory: false,
                is_file: true,
            }]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_file_system_stats_paths() {
        let fs = test_fs().await;

        let root = fs.get_metadata(&FsPath::root()).await.expect("stat root");
        assert!(root.is_directory);
        assert!(!root.is_file);
        assert!(!root.is_symlink);
        assert_eq!(root.created_at_ms, 0);
        assert_eq!(root.modified_at_ms, 0);

        let file = fs
            .get_metadata(&FsPath::new("/README.md").unwrap())
            .await
            .expect("stat file");
        assert!(file.is_file);
        assert!(!file.is_directory);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_file_system_rejects_writes() {
        let fs = test_fs().await;

        assert_eq!(fs.access_policy(), FileAccessPolicy::FullReadOnly);
        assert!(matches!(
            fs.write_file(&FsPath::new("/README.md").unwrap(), b"updated".to_vec())
                .await,
            Err(FsError::PermissionDenied { .. })
        ));
        assert!(matches!(
            fs.create_directory(
                &FsPath::new("/generated").unwrap(),
                CreateDirectoryOptions::recursive()
            )
            .await,
            Err(FsError::PermissionDenied { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_file_system_maps_missing_and_wrong_kind_errors() {
        let fs = test_fs().await;

        assert!(matches!(
            fs.read_file(&FsPath::new("/missing.txt").unwrap()).await,
            Err(FsError::NotFound { .. })
        ));
        assert!(matches!(
            fs.read_file(&FsPath::new("/src").unwrap()).await,
            Err(FsError::InvalidInput { .. })
        ));
        assert!(matches!(
            fs.read_directory(&FsPath::new("/README.md").unwrap()).await,
            Err(FsError::InvalidInput { .. })
        ));
        assert!(matches!(
            fs.read_file(&FsPath::new("../escape").unwrap()).await,
            Err(FsError::InvalidInput { .. })
        ));
    }
}
