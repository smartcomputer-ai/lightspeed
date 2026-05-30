//! Filesystem adapters over CAS-backed VFS snapshots and workspaces.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

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
        fs_path_to_vfs_path(path)
    }

    fn deny_write(&self, path: &FsPath) -> FsError {
        FsError::PermissionDenied { path: path.clone() }
    }
}

#[derive(Clone)]
pub struct VfsWorkspaceFileSystem {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn ::vfs::VfsWorkspaceStore>,
    workspace_id: ::vfs::VfsWorkspaceId,
}

impl VfsWorkspaceFileSystem {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn ::vfs::VfsWorkspaceStore>,
        workspace_id: ::vfs::VfsWorkspaceId,
    ) -> Self {
        Self {
            blobs,
            workspace_store,
            workspace_id,
        }
    }

    pub fn workspace_id(&self) -> &::vfs::VfsWorkspaceId {
        &self.workspace_id
    }

    async fn read_head(&self) -> FsResult<(::vfs::VfsWorkspaceRecord, ::vfs::VfsSnapshotManifest)> {
        let record = self
            .workspace_store
            .read_workspace(&self.workspace_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let manifest =
            ::vfs::read_snapshot_manifest(self.blobs.as_ref(), &record.head_snapshot_ref)
                .await
                .map_err(|error| map_vfs_error(error, &FsPath::root()))?;
        Ok((record, manifest))
    }

    async fn commit_head(
        &self,
        current: ::vfs::VfsWorkspaceRecord,
        manifest: ::vfs::VfsSnapshotManifest,
    ) -> FsResult<()> {
        let result = ::vfs::commit_snapshot_manifest(self.blobs.as_ref(), manifest)
            .await
            .map_err(|error| map_vfs_error(error, &FsPath::root()))?;
        self.workspace_store
            .compare_and_set_head(::vfs::CompareAndSetVfsWorkspaceHead {
                workspace_id: current.workspace_id,
                expected_revision: current.revision,
                new_head_snapshot_ref: result.snapshot_ref,
                updated_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)?;
        Ok(())
    }

    async fn update_head<F>(&self, request_path: &FsPath, update: F) -> FsResult<()>
    where
        F: FnOnce(&mut ::vfs::VfsSnapshotManifest) -> Result<(), ::vfs::VfsError>,
    {
        let (record, mut manifest) = self.read_head().await?;
        update(&mut manifest).map_err(|error| map_vfs_error(error, request_path))?;
        self.commit_head(record, manifest).await
    }

    async fn update_head_async<F, Fut>(&self, request_path: &FsPath, update: F) -> FsResult<()>
    where
        F: FnOnce(::vfs::VfsSnapshotManifest) -> Fut,
        Fut: std::future::Future<Output = Result<::vfs::VfsSnapshotManifest, ::vfs::VfsError>>,
    {
        let (record, manifest) = self.read_head().await?;
        let manifest = update(manifest)
            .await
            .map_err(|error| map_vfs_error(error, request_path))?;
        self.commit_head(record, manifest).await
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

#[async_trait]
impl FileSystem for VfsWorkspaceFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        FileAccessPolicy::FullReadWrite
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        let (_record, manifest) = self.read_head().await?;
        ::vfs::read_snapshot_file(self.blobs.as_ref(), &manifest, &vfs_path)
            .await
            .map_err(|error| map_vfs_error(error, path))
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        self.update_head_async(path, |mut manifest| {
            let blobs = Arc::clone(&self.blobs);
            async move {
                ::vfs::write_manifest_file(
                    blobs.as_ref(),
                    &mut manifest,
                    &vfs_path,
                    contents,
                    None,
                    false,
                )
                .await?;
                Ok(manifest)
            }
        })
        .await
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        self.update_head(path, |manifest| {
            ::vfs::create_manifest_directory(manifest, &vfs_path, options.recursive)
        })
        .await
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        let (_record, manifest) = self.read_head().await?;
        let metadata = ::vfs::stat_snapshot_path(&manifest, &vfs_path)
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
        let vfs_path = fs_path_to_vfs_path(path)?;
        let (_record, manifest) = self.read_head().await?;
        ::vfs::list_snapshot_directory(&manifest, &vfs_path)
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

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        self.update_head(path, |manifest| {
            ::vfs::remove_manifest_path(manifest, &vfs_path, options.recursive, options.force)
        })
        .await
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        let source = fs_path_to_vfs_path(source_path)?;
        let destination = fs_path_to_vfs_path(destination_path)?;
        self.update_head(destination_path, |manifest| {
            ::vfs::copy_manifest_path(manifest, &source, &destination, options.recursive)
        })
        .await
    }
}

fn fs_path_to_vfs_path(path: &FsPath) -> FsResult<::vfs::VfsPath> {
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

fn now_ms() -> FsResult<i64> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| FsError::Failed {
            message: format!("system clock is before unix epoch: {error}"),
        })?
        .as_millis();
    i64::try_from(ms).map_err(|_| FsError::Failed {
        message: "current timestamp does not fit in i64 milliseconds".to_owned(),
    })
}

fn map_vfs_error(error: ::vfs::VfsError, request_path: &FsPath) -> FsError {
    match error {
        ::vfs::VfsError::NotFound { .. } => FsError::NotFound {
            path: request_path.clone(),
        },
        ::vfs::VfsError::AlreadyExists { .. } => FsError::AlreadyExists {
            path: request_path.clone(),
        },
        ::vfs::VfsError::NotAFile { path } => FsError::InvalidInput {
            message: format!("path is not a file: {path}"),
        },
        ::vfs::VfsError::NotADirectory { path } => FsError::InvalidInput {
            message: format!("path is not a directory: {path}"),
        },
        ::vfs::VfsError::DirectoryNotEmpty { path } => FsError::InvalidInput {
            message: format!("directory is not empty: {path}"),
        },
        ::vfs::VfsError::InvalidOperation { message } => FsError::InvalidInput { message },
        ::vfs::VfsError::PathConflict { path, existing } => FsError::InvalidInput {
            message: format!("path conflicts with an existing {existing}: {path}"),
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

fn map_vfs_catalog_error(error: ::vfs::VfsCatalogError) -> FsError {
    match error {
        ::vfs::VfsCatalogError::AlreadyExists { id, .. } => FsError::AlreadyExists {
            path: FsPath::new(format!("/{}", id)).unwrap_or_else(|_| FsPath::root()),
        },
        ::vfs::VfsCatalogError::NotFound { id, .. } => FsError::NotFound {
            path: FsPath::new(format!("/{}", id)).unwrap_or_else(|_| FsPath::root()),
        },
        ::vfs::VfsCatalogError::RevisionConflict {
            workspace_id,
            expected_revision,
            actual_revision,
        } => FsError::Failed {
            message: format!(
                "vfs workspace revision conflict for {workspace_id}: expected {expected_revision}, actual {actual_revision}"
            ),
        },
        ::vfs::VfsCatalogError::InvalidInput { message } => FsError::InvalidInput { message },
        ::vfs::VfsCatalogError::Store { message } => FsError::Failed { message },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ::vfs::VfsWorkspaceStore;
    use async_trait::async_trait;
    use engine::storage::InMemoryBlobStore;
    use tokio::sync::Mutex;

    use super::*;
    use crate::host::{
        context::HostToolContext,
        tools::{
            ApplyPatchArgs, EditFileArgs, GlobArgs, GrepArgs, ListDirArgs, ReadFileArgs,
            WriteFileArgs, invoke_apply_patch, invoke_edit_file, invoke_glob, invoke_grep,
            invoke_list_dir, invoke_read_file, invoke_write_file,
        },
    };

    #[derive(Debug, Default)]
    struct TestWorkspaceStore {
        records: Mutex<BTreeMap<::vfs::VfsWorkspaceId, ::vfs::VfsWorkspaceRecord>>,
        force_revision_conflict: bool,
    }

    impl TestWorkspaceStore {
        fn with_forced_revision_conflict() -> Self {
            Self {
                records: Mutex::new(BTreeMap::new()),
                force_revision_conflict: true,
            }
        }
    }

    #[async_trait]
    impl ::vfs::VfsWorkspaceStore for TestWorkspaceStore {
        async fn create_workspace(
            &self,
            record: ::vfs::CreateVfsWorkspaceRecord,
        ) -> Result<::vfs::VfsWorkspaceRecord, ::vfs::VfsCatalogError> {
            let mut records = self.records.lock().await;
            if records.contains_key(&record.workspace_id) {
                return Err(::vfs::VfsCatalogError::AlreadyExists {
                    kind: "workspace",
                    id: record.workspace_id.to_string(),
                });
            }
            let record = ::vfs::VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            };
            records.insert(record.workspace_id.clone(), record.clone());
            Ok(record)
        }

        async fn read_workspace(
            &self,
            workspace_id: &::vfs::VfsWorkspaceId,
        ) -> Result<::vfs::VfsWorkspaceRecord, ::vfs::VfsCatalogError> {
            self.records
                .lock()
                .await
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| ::vfs::VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }

        async fn compare_and_set_head(
            &self,
            request: ::vfs::CompareAndSetVfsWorkspaceHead,
        ) -> Result<::vfs::VfsWorkspaceRecord, ::vfs::VfsCatalogError> {
            let mut records = self.records.lock().await;
            let record = records.get_mut(&request.workspace_id).ok_or_else(|| {
                ::vfs::VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                }
            })?;
            if self.force_revision_conflict || record.revision != request.expected_revision {
                return Err(::vfs::VfsCatalogError::RevisionConflict {
                    workspace_id: request.workspace_id,
                    expected_revision: request.expected_revision,
                    actual_revision: record.revision + u64::from(self.force_revision_conflict),
                });
            }
            record.head_snapshot_ref = request.new_head_snapshot_ref;
            record.revision += 1;
            record.updated_at_ms = request.updated_at_ms;
            Ok(record.clone())
        }
    }

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

    async fn test_workspace_fs(
        store: Arc<TestWorkspaceStore>,
        files: Vec<::vfs::InlineFile>,
    ) -> (
        Arc<InMemoryBlobStore>,
        VfsWorkspaceFileSystem,
        ::vfs::VfsWorkspaceId,
        BlobRef,
    ) {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let snapshot = ::vfs::create_inline_snapshot(
            blobs.as_ref(),
            ::vfs::CreateInlineSnapshotRequest::new(files),
        )
        .await
        .expect("create base snapshot");
        let workspace_id = ::vfs::VfsWorkspaceId::new("workspace_1");
        store
            .create_workspace(::vfs::CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                base_snapshot_ref: Some(snapshot.snapshot_ref.clone()),
                head_snapshot_ref: snapshot.snapshot_ref.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create workspace");
        let fs = VfsWorkspaceFileSystem::new(blobs.clone(), store, workspace_id.clone());
        (blobs, fs, workspace_id, snapshot.snapshot_ref)
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

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_workspace_file_system_writes_new_snapshot_heads() {
        let store = Arc::new(TestWorkspaceStore::default());
        let (blobs, fs, workspace_id, base_snapshot_ref) = test_workspace_fs(
            store.clone(),
            vec![::vfs::InlineFile::new("README.md", b"base\n".to_vec()).unwrap()],
        )
        .await;

        assert_eq!(fs.access_policy(), FileAccessPolicy::FullReadWrite);
        fs.create_directory(
            &FsPath::new("/src").unwrap(),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create src");
        fs.write_file(
            &FsPath::new("/src/lib.rs").unwrap(),
            b"pub fn f() {}\n".to_vec(),
        )
        .await
        .expect("write lib");

        assert_eq!(
            fs.read_file_text(&FsPath::new("/src/lib.rs").unwrap())
                .await
                .expect("read lib"),
            "pub fn f() {}\n"
        );
        let reloaded_fs =
            VfsWorkspaceFileSystem::new(blobs.clone(), store.clone(), workspace_id.clone());
        assert_eq!(
            reloaded_fs
                .read_file_text(&FsPath::new("/src/lib.rs").unwrap())
                .await
                .expect("read lib after reloading workspace head"),
            "pub fn f() {}\n"
        );
        let metadata = fs
            .get_metadata(&FsPath::new("/src/lib.rs").unwrap())
            .await
            .expect("stat lib");
        assert!(metadata.is_file);
        assert!(!metadata.is_symlink);

        let record = store
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        assert_eq!(record.revision, 2);
        assert_ne!(record.head_snapshot_ref, base_snapshot_ref);

        let base_manifest = ::vfs::read_snapshot_manifest(blobs.as_ref(), &base_snapshot_ref)
            .await
            .expect("read base manifest");
        assert!(matches!(
            ::vfs::lookup_snapshot_path(&base_manifest, &::vfs::VfsPath::parse("/src").unwrap()),
            Err(::vfs::VfsError::NotFound { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_workspace_file_system_removes_and_copies_paths() {
        let store = Arc::new(TestWorkspaceStore::default());
        let (_blobs, fs, _workspace_id, _base_snapshot_ref) = test_workspace_fs(
            store,
            vec![::vfs::InlineFile::new("src/lib.rs", b"pub fn f() {}\n".to_vec()).unwrap()],
        )
        .await;

        fs.copy(
            &FsPath::new("/src").unwrap(),
            &FsPath::new("/copy").unwrap(),
            CopyOptions::recursive(),
        )
        .await
        .expect("copy dir");
        assert_eq!(
            fs.read_file_text(&FsPath::new("/copy/lib.rs").unwrap())
                .await
                .expect("read copied file"),
            "pub fn f() {}\n"
        );
        assert!(matches!(
            fs.copy(
                &FsPath::new("/copy/lib.rs").unwrap(),
                &FsPath::new("/copy").unwrap(),
                CopyOptions::file()
            )
            .await,
            Err(FsError::InvalidInput { .. })
        ));

        assert!(matches!(
            fs.remove(&FsPath::new("/src").unwrap(), RemoveOptions::file())
                .await,
            Err(FsError::InvalidInput { .. })
        ));
        fs.remove(&FsPath::new("/src").unwrap(), RemoveOptions::recursive())
            .await
            .expect("remove src");
        assert!(matches!(
            fs.read_directory(&FsPath::new("/src").unwrap()).await,
            Err(FsError::NotFound { .. })
        ));
        fs.remove(
            &FsPath::new("/missing").unwrap(),
            RemoveOptions::file().force(),
        )
        .await
        .expect("force remove missing");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_workspace_file_system_reports_revision_conflicts() {
        let store = Arc::new(TestWorkspaceStore::with_forced_revision_conflict());
        let (_blobs, fs, _workspace_id, _base_snapshot_ref) = test_workspace_fs(
            store,
            vec![::vfs::InlineFile::new("README.md", b"base\n".to_vec()).unwrap()],
        )
        .await;

        let error = fs
            .write_file(&FsPath::new("/README.md").unwrap(), b"updated\n".to_vec())
            .await
            .expect_err("write should conflict");
        assert!(
            matches!(error, FsError::Failed { message } if message.contains("revision conflict"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn existing_file_tools_work_against_vfs_workspace_file_system() {
        let store = Arc::new(TestWorkspaceStore::default());
        let (blobs, fs, _workspace_id, _base_snapshot_ref) =
            test_workspace_fs(store, Vec::new()).await;
        let ctx = HostToolContext::new(Arc::new(fs.clone()), None, blobs)
            .with_cwd(FsPath::new("/workspace").unwrap());

        let write = invoke_write_file(
            &ctx,
            WriteFileArgs {
                path: FsPath::new("src/lib.rs").unwrap(),
                content: "pub fn alpha() {}\n".to_owned(),
            },
        )
        .await
        .expect("write through tool");
        assert_eq!(
            write.resolved_path,
            FsPath::new("/workspace/src/lib.rs").unwrap()
        );

        let read = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("src/lib.rs").unwrap(),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read through tool");
        assert_eq!(read.text, "pub fn alpha() {}");

        invoke_edit_file(
            &ctx,
            EditFileArgs {
                path: FsPath::new("src/lib.rs").unwrap(),
                old_string: "alpha".to_owned(),
                new_string: "beta".to_owned(),
                replace_all: false,
            },
        )
        .await
        .expect("edit through tool");

        invoke_apply_patch(
            &ctx,
            ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: src/main.rs\n+fn main() {}\n*** End Patch"
                    .to_owned(),
            },
        )
        .await
        .expect("apply patch through tool");

        let grep = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "beta".to_owned(),
                path: Some(FsPath::current_dir()),
                include: Some("*.rs".to_owned()),
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep through tool");
        assert_eq!(grep.matches.len(), 1);
        assert_eq!(
            grep.matches[0].path,
            FsPath::new("/workspace/src/lib.rs").unwrap()
        );

        let glob = invoke_glob(
            &ctx,
            GlobArgs {
                pattern: "**/*.rs".to_owned(),
                path: Some(FsPath::current_dir()),
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("glob through tool");
        assert_eq!(
            glob.matches,
            vec![
                FsPath::new("/workspace/src/lib.rs").unwrap(),
                FsPath::new("/workspace/src/main.rs").unwrap(),
            ]
        );

        let listing = invoke_list_dir(
            &ctx,
            ListDirArgs {
                path: FsPath::new("src").unwrap(),
            },
        )
        .await
        .expect("list through tool");
        assert_eq!(listing.entries.len(), 2);
    }
}
