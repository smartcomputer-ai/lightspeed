//! Filesystem adapters over CAS-backed VFS snapshots and workspaces.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use engine::{
    BlobRef, ToolEffect,
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
    effects: ToolEffectLog,
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
            effects: ToolEffectLog::default(),
        }
    }

    fn with_effect_log(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn ::vfs::VfsWorkspaceStore>,
        workspace_id: ::vfs::VfsWorkspaceId,
        effects: ToolEffectLog,
    ) -> Self {
        Self {
            blobs,
            workspace_store,
            workspace_id,
            effects,
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
        let updated = self
            .workspace_store
            .compare_and_set_head(::vfs::CompareAndSetVfsWorkspaceHead {
                workspace_id: current.workspace_id,
                expected_revision: Some(current.revision),
                new_head_snapshot_ref: result.snapshot_ref,
                updated_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)?;
        self.effects.record(vfs_workspace_commit_effect(&updated));
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

#[derive(Clone)]
pub struct MountedVfsFileSystem {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn ::vfs::VfsWorkspaceStore>,
    mounts: Arc<Vec<::vfs::VfsMountRecord>>,
    effects: ToolEffectLog,
}

#[derive(Clone, Debug)]
struct ResolvedMount {
    mount: ::vfs::VfsMountRecord,
    inner_path: FsPath,
}

const VFS_WORKSPACE_COMMIT_EFFECT_KIND: &str = "forge.vfs.workspace_commit.v1";

#[derive(Clone, Default)]
struct ToolEffectLog {
    effects: Arc<Mutex<Vec<ToolEffect>>>,
}

impl ToolEffectLog {
    fn record(&self, effect: ToolEffect) {
        self.effects
            .lock()
            .expect("tool effect log poisoned")
            .push(effect);
    }

    fn drain(&self) -> Vec<ToolEffect> {
        self.effects
            .lock()
            .expect("tool effect log poisoned")
            .drain(..)
            .collect()
    }
}

impl MountedVfsFileSystem {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn ::vfs::VfsWorkspaceStore>,
        mut mounts: Vec<::vfs::VfsMountRecord>,
    ) -> FsResult<Self> {
        validate_mounts(&mounts)?;
        mounts.sort_by(|left, right| {
            right
                .mount_path
                .depth()
                .cmp(&left.mount_path.depth())
                .then_with(|| left.mount_path.cmp(&right.mount_path))
        });
        Ok(Self {
            blobs,
            workspace_store,
            mounts: Arc::new(mounts),
            effects: ToolEffectLog::default(),
        })
    }

    pub fn from_mount_table(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn ::vfs::VfsWorkspaceStore>,
        mount_table: ::vfs::VfsMountTable,
    ) -> FsResult<Self> {
        Self::new(blobs, workspace_store, mount_table.mounts)
    }

    pub fn mounts(&self) -> &[::vfs::VfsMountRecord] {
        self.mounts.as_slice()
    }

    fn resolve_mount(&self, path: &FsPath) -> FsResult<Option<ResolvedMount>> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        for mount in self.mounts.iter() {
            if vfs_path_starts_with(&vfs_path, &mount.mount_path) {
                return Ok(Some(ResolvedMount {
                    mount: mount.clone(),
                    inner_path: vfs_path_to_fs_path(&strip_mount_path(
                        &vfs_path,
                        &mount.mount_path,
                    )?)?,
                }));
            }
        }
        Ok(None)
    }

    async fn file_system_for_mount(
        &self,
        mount: &::vfs::VfsMountRecord,
        request_path: &FsPath,
    ) -> FsResult<Box<dyn FileSystem>> {
        match &mount.source {
            ::vfs::VfsMountSource::Snapshot { snapshot_ref } => {
                let fs = VfsSnapshotFileSystem::new(self.blobs.clone(), snapshot_ref.clone())
                    .await
                    .map_err(|error| map_vfs_error(error, request_path))?;
                Ok(Box::new(fs))
            }
            ::vfs::VfsMountSource::Workspace { workspace_id } => {
                Ok(Box::new(VfsWorkspaceFileSystem::with_effect_log(
                    self.blobs.clone(),
                    self.workspace_store.clone(),
                    workspace_id.clone(),
                    self.effects.clone(),
                )))
            }
        }
    }

    fn writable_workspace_for_mount(
        &self,
        mount: &::vfs::VfsMountRecord,
        request_path: &FsPath,
    ) -> FsResult<VfsWorkspaceFileSystem> {
        if !mount.access.is_writable() {
            return Err(FsError::PermissionDenied {
                path: request_path.clone(),
            });
        }
        match &mount.source {
            ::vfs::VfsMountSource::Workspace { workspace_id } => {
                Ok(VfsWorkspaceFileSystem::with_effect_log(
                    self.blobs.clone(),
                    self.workspace_store.clone(),
                    workspace_id.clone(),
                    self.effects.clone(),
                ))
            }
            ::vfs::VfsMountSource::Snapshot { .. } => Err(FsError::PermissionDenied {
                path: request_path.clone(),
            }),
        }
    }

    fn synthetic_directory_entries(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let vfs_path = fs_path_to_vfs_path(path)?;
        let mut entries = BTreeMap::new();
        for mount in self.mounts.iter() {
            if let Some(file_name) = immediate_mount_child(&vfs_path, &mount.mount_path) {
                entries.insert(
                    file_name.to_owned(),
                    ReadDirectoryEntry {
                        file_name: file_name.to_owned(),
                        is_directory: true,
                        is_file: false,
                    },
                );
            }
        }
        Ok(entries.into_values().collect())
    }

    fn synthetic_metadata(&self, path: &FsPath) -> FsResult<Option<FileMetadata>> {
        if self.synthetic_directory_entries(path)?.is_empty() {
            return Ok(None);
        }
        Ok(Some(directory_metadata()))
    }

    async fn copy_generic(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        let source_path = fs_path_key(source_path)?;
        let destination_path = fs_path_key(destination_path)?;
        let source_metadata = self.get_metadata(&source_path).await?;
        if source_metadata.is_file {
            let bytes = self.read_file(&source_path).await?;
            return self.write_file(&destination_path, bytes).await;
        }
        if !source_metadata.is_directory {
            return Err(FsError::InvalidInput {
                message: format!("path is neither a file nor a directory: {source_path}"),
            });
        }
        if !options.recursive {
            return Err(FsError::InvalidInput {
                message: "copy requires recursive: true when source is a directory".to_owned(),
            });
        }
        if destination_path.starts_with(&source_path) {
            return Err(FsError::InvalidInput {
                message: "cannot copy a directory to itself or one of its descendants".to_owned(),
            });
        }
        self.copy_directory_generic(&source_path, &destination_path)
            .await
    }

    async fn copy_directory_generic(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
    ) -> FsResult<()> {
        let mut stack = vec![(source_path.clone(), destination_path.clone(), false)];
        while let Some((source, destination, visited)) = stack.pop() {
            if visited {
                let bytes = self.read_file(&source).await?;
                self.write_file(&destination, bytes).await?;
                continue;
            }

            let metadata = self.get_metadata(&source).await?;
            if metadata.is_file {
                stack.push((source, destination, true));
                continue;
            }

            self.remove(&destination, RemoveOptions::recursive().force())
                .await?;
            self.create_directory(&destination, CreateDirectoryOptions::single())
                .await?;

            let mut entries = self.read_directory(&source).await?;
            entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));
            for entry in entries {
                let source_child = source.join(&entry.file_name)?;
                let destination_child = destination.join(&entry.file_name)?;
                stack.push((source_child, destination_child, entry.is_file));
            }
        }
        Ok(())
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

    fn drain_tool_effects(&self) -> Vec<ToolEffect> {
        self.effects.drain()
    }
}

#[async_trait]
impl FileSystem for MountedVfsFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        if self.mounts.iter().any(|mount| mount.access.is_writable()) {
            FileAccessPolicy::FullReadWrite
        } else {
            FileAccessPolicy::FullReadOnly
        }
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        if let Some(resolved) = self.resolve_mount(path)? {
            let fs = self.file_system_for_mount(&resolved.mount, path).await?;
            return fs.read_file(&resolved.inner_path).await;
        }
        if self.synthetic_metadata(path)?.is_some() {
            return Err(FsError::InvalidInput {
                message: format!("path is not a file: {path}"),
            });
        }
        Err(FsError::NotFound { path: path.clone() })
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        let Some(resolved) = self.resolve_mount(path)? else {
            return Err(FsError::PermissionDenied { path: path.clone() });
        };
        let fs = self.writable_workspace_for_mount(&resolved.mount, path)?;
        fs.write_file(&resolved.inner_path, contents).await
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        if let Some(resolved) = self.resolve_mount(path)? {
            let fs = self.writable_workspace_for_mount(&resolved.mount, path)?;
            return fs.create_directory(&resolved.inner_path, options).await;
        }
        if self.synthetic_metadata(path)?.is_some() {
            return if options.recursive {
                Ok(())
            } else {
                Err(FsError::AlreadyExists { path: path.clone() })
            };
        }
        Err(FsError::PermissionDenied { path: path.clone() })
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        if let Some(resolved) = self.resolve_mount(path)? {
            let fs = self.file_system_for_mount(&resolved.mount, path).await?;
            return fs.get_metadata(&resolved.inner_path).await;
        }
        if let Some(metadata) = self.synthetic_metadata(path)? {
            return Ok(metadata);
        }
        Err(FsError::NotFound { path: path.clone() })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        if let Some(resolved) = self.resolve_mount(path)? {
            let fs = self.file_system_for_mount(&resolved.mount, path).await?;
            return fs.read_directory(&resolved.inner_path).await;
        }
        let entries = self.synthetic_directory_entries(path)?;
        if !entries.is_empty() {
            return Ok(entries);
        }
        Err(FsError::NotFound { path: path.clone() })
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        let Some(resolved) = self.resolve_mount(path)? else {
            return Err(FsError::PermissionDenied { path: path.clone() });
        };
        let fs = self.writable_workspace_for_mount(&resolved.mount, path)?;
        fs.remove(&resolved.inner_path, options).await
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        if let (Some(source), Some(destination)) = (
            self.resolve_mount(source_path)?,
            self.resolve_mount(destination_path)?,
        ) && source.mount.mount_path == destination.mount.mount_path
        {
            let fs = self.writable_workspace_for_mount(&destination.mount, destination_path)?;
            return fs
                .copy(&source.inner_path, &destination.inner_path, options)
                .await;
        }
        self.copy_generic(source_path, destination_path, options)
            .await
    }

    fn drain_tool_effects(&self) -> Vec<ToolEffect> {
        self.effects.drain()
    }
}

fn fs_path_to_vfs_path(path: &FsPath) -> FsResult<::vfs::VfsPath> {
    let path = fs_path_key(path)?;
    if path.is_root() {
        return Ok(::vfs::VfsPath::root());
    }
    ::vfs::VfsPath::parse(path.as_str()).map_err(|error| FsError::InvalidInput {
        message: error.to_string(),
    })
}

fn fs_path_key(path: &FsPath) -> FsResult<FsPath> {
    if path.has_unresolved_parent() {
        return Err(FsError::InvalidInput {
            message: format!("vfs path cannot contain unresolved parent components: {path}"),
        });
    }
    if path.is_absolute() {
        Ok(path.clone())
    } else if path.is_root() {
        Ok(FsPath::root())
    } else {
        FsPath::new(format!("/{}", path.as_str())).map_err(Into::into)
    }
}

fn vfs_path_to_fs_path(path: &::vfs::VfsPath) -> FsResult<FsPath> {
    FsPath::new(path.as_str()).map_err(Into::into)
}

fn strip_mount_path(
    path: &::vfs::VfsPath,
    mount_path: &::vfs::VfsPath,
) -> FsResult<::vfs::VfsPath> {
    if mount_path.is_root() {
        return Ok(path.clone());
    }
    if path == mount_path {
        return Ok(::vfs::VfsPath::root());
    }
    let suffix = path
        .as_str()
        .strip_prefix(mount_path.as_str())
        .ok_or_else(|| FsError::InvalidInput {
            message: format!("path {path} is not under mount {mount_path}"),
        })?;
    ::vfs::VfsPath::parse(suffix).map_err(|error| FsError::InvalidInput {
        message: error.to_string(),
    })
}

fn validate_mounts(mounts: &[::vfs::VfsMountRecord]) -> FsResult<()> {
    let mut seen = BTreeSet::new();
    for mount in mounts {
        if !seen.insert(mount.mount_path.clone()) {
            return Err(FsError::InvalidInput {
                message: format!("duplicate vfs mount path: {}", mount.mount_path),
            });
        }
        if mount.access.is_writable()
            && matches!(mount.source, ::vfs::VfsMountSource::Snapshot { .. })
        {
            return Err(FsError::InvalidInput {
                message: format!("snapshot mount cannot be writable: {}", mount.mount_path),
            });
        }
    }

    let mounts = mounts.iter().collect::<Vec<_>>();
    for (index, left) in mounts.iter().enumerate() {
        for right in mounts.iter().skip(index + 1) {
            if vfs_path_starts_with(&left.mount_path, &right.mount_path)
                || vfs_path_starts_with(&right.mount_path, &left.mount_path)
            {
                return Err(FsError::InvalidInput {
                    message: format!(
                        "nested vfs mounts are not supported: {} and {}",
                        left.mount_path, right.mount_path
                    ),
                });
            }
        }
    }
    Ok(())
}

fn immediate_mount_child<'a>(
    parent: &::vfs::VfsPath,
    mount_path: &'a ::vfs::VfsPath,
) -> Option<&'a str> {
    let parent_components = parent.components();
    let mount_components = mount_path.components();
    if parent_components.len() >= mount_components.len() {
        return None;
    }
    if parent_components
        .iter()
        .zip(mount_components.iter())
        .all(|(left, right)| left == right)
    {
        Some(mount_components[parent_components.len()])
    } else {
        None
    }
}

fn vfs_path_starts_with(path: &::vfs::VfsPath, base: &::vfs::VfsPath) -> bool {
    if base.is_root() {
        return true;
    }
    path == base
        || path
            .as_str()
            .strip_prefix(base.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn directory_metadata() -> FileMetadata {
    FileMetadata {
        is_directory: true,
        is_file: false,
        is_symlink: false,
        created_at_ms: 0,
        modified_at_ms: 0,
    }
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

fn vfs_workspace_commit_effect(record: &::vfs::VfsWorkspaceRecord) -> ToolEffect {
    ToolEffect {
        kind: VFS_WORKSPACE_COMMIT_EFFECT_KIND.to_owned(),
        data: BTreeMap::from([
            (
                "workspace_id".to_owned(),
                record.workspace_id.as_str().to_owned(),
            ),
            (
                "snapshot_ref".to_owned(),
                record.head_snapshot_ref.as_str().to_owned(),
            ),
            ("revision".to_owned(), record.revision.to_string()),
        ]),
    }
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
    use engine::{
        CoreAgentTools, RunId, SessionId, ToolBatchId, ToolCallId, ToolCallStatus,
        ToolInvocationBatchRequest, ToolInvocationRequest, ToolName, TurnId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use tokio::sync::Mutex;

    use super::*;
    use crate::host::{
        context::HostToolContext,
        executor::InlineHostToolRuntime,
        targets::HostToolTargets,
        tools::{
            ApplyPatchArgs, EditFileArgs, GlobArgs, GrepArgs, ListDirArgs, ReadFileArgs,
            WriteFileArgs, invoke_apply_patch, invoke_edit_file, invoke_glob, invoke_grep,
            invoke_list_dir, invoke_read_file, invoke_write_file,
        },
    };
    use crate::runtime::ToolTarget;
    use crate::toolset::{ToolsetConfig, ToolsetEnvironment, resolve_toolset};

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
            let revision_conflict = request
                .expected_revision
                .is_some_and(|expected_revision| record.revision != expected_revision);
            if self.force_revision_conflict || revision_conflict {
                return Err(::vfs::VfsCatalogError::RevisionConflict {
                    workspace_id: request.workspace_id,
                    expected_revision: request.expected_revision.unwrap_or(record.revision),
                    actual_revision: record.revision + u64::from(self.force_revision_conflict),
                });
            }
            record.head_snapshot_ref = request.new_head_snapshot_ref;
            record.revision += 1;
            record.updated_at_ms = request.updated_at_ms;
            Ok(record.clone())
        }

        async fn delete_workspace(
            &self,
            workspace_id: &::vfs::VfsWorkspaceId,
        ) -> Result<::vfs::VfsWorkspaceRecord, ::vfs::VfsCatalogError> {
            self.records
                .lock()
                .await
                .remove(workspace_id)
                .ok_or_else(|| ::vfs::VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
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

    async fn create_test_snapshot(
        blobs: &InMemoryBlobStore,
        files: Vec<::vfs::InlineFile>,
    ) -> ::vfs::CreateVfsSnapshotResult {
        ::vfs::create_inline_snapshot(blobs, ::vfs::CreateInlineSnapshotRequest::new(files))
            .await
            .expect("create snapshot")
    }

    async fn create_test_workspace(
        store: &TestWorkspaceStore,
        workspace_id: &str,
        snapshot_ref: BlobRef,
    ) -> ::vfs::VfsWorkspaceId {
        let workspace_id = ::vfs::VfsWorkspaceId::new(workspace_id);
        store
            .create_workspace(::vfs::CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                base_snapshot_ref: Some(snapshot_ref.clone()),
                head_snapshot_ref: snapshot_ref,
                created_at_ms: 1,
            })
            .await
            .expect("create workspace");
        workspace_id
    }

    fn mount_record(
        session_id: &SessionId,
        mount_path: &str,
        source: ::vfs::VfsMountSource,
        access: ::vfs::VfsMountAccess,
    ) -> ::vfs::VfsMountRecord {
        ::vfs::VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: ::vfs::VfsPath::parse(mount_path).unwrap(),
            source,
            access,
        }
    }

    async fn test_mounted_fs() -> (
        Arc<InMemoryBlobStore>,
        Arc<TestWorkspaceStore>,
        MountedVfsFileSystem,
        ::vfs::VfsWorkspaceId,
    ) {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let store = Arc::new(TestWorkspaceStore::default());
        let session_id = SessionId::new("session_1");

        let skill_snapshot = create_test_snapshot(
            blobs.as_ref(),
            vec![
                ::vfs::InlineFile::new("SKILL.md", b"# Rust Skill\n".to_vec()).unwrap(),
                ::vfs::InlineFile::new("references/info.md", b"reference\n".to_vec()).unwrap(),
            ],
        )
        .await;
        let workspace_snapshot = create_test_snapshot(blobs.as_ref(), Vec::new()).await;
        let workspace_id = create_test_workspace(
            store.as_ref(),
            "workspace_1",
            workspace_snapshot.snapshot_ref,
        )
        .await;
        let fs = MountedVfsFileSystem::new(
            blobs.clone(),
            store.clone(),
            vec![
                mount_record(
                    &session_id,
                    "/skills/rust",
                    ::vfs::VfsMountSource::Snapshot {
                        snapshot_ref: skill_snapshot.snapshot_ref,
                    },
                    ::vfs::VfsMountAccess::ReadOnly,
                ),
                mount_record(
                    &session_id,
                    "/workspace",
                    ::vfs::VfsMountSource::Workspace {
                        workspace_id: workspace_id.clone(),
                    },
                    ::vfs::VfsMountAccess::ReadWrite,
                ),
            ],
        )
        .expect("mounted fs");

        (blobs, store, fs, workspace_id)
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
    async fn vfs_workspace_file_system_records_commit_effects() {
        let store = Arc::new(TestWorkspaceStore::default());
        let (_blobs, fs, workspace_id, _base_snapshot_ref) =
            test_workspace_fs(store.clone(), Vec::new()).await;

        fs.write_file(&FsPath::new("/README.md").unwrap(), b"updated\n".to_vec())
            .await
            .expect("write file");

        let record = store
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        let effects = fs.drain_tool_effects();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, VFS_WORKSPACE_COMMIT_EFFECT_KIND);
        assert_eq!(
            effects[0].data.get("workspace_id").map(String::as_str),
            Some(workspace_id.as_str())
        );
        assert_eq!(
            effects[0].data.get("snapshot_ref").map(String::as_str),
            Some(record.head_snapshot_ref.as_str())
        );
        assert_eq!(
            effects[0].data.get("revision").map(String::as_str),
            Some("1")
        );
        assert!(fs.drain_tool_effects().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_runtime_surfaces_vfs_workspace_commit_effects() {
        let store = Arc::new(TestWorkspaceStore::default());
        let (blobs, fs, workspace_id, _base_snapshot_ref) =
            test_workspace_fs(store.clone(), Vec::new()).await;
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
        let target = ToolTarget::api_kind(engine::ProviderApiKind::OpenAiResponses);
        let toolset = resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");
        let runtime = InlineHostToolRuntime::new(ctx, toolset.catalog);
        let args_ref = blobs
            .put_bytes(br#"{"path":"/README.md","content":"updated\n"}"#.to_vec())
            .await
            .expect("write args");

        let result = CoreAgentTools::invoke_batch(
            &runtime,
            ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                calls: vec![ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("write_file"),
                    arguments_ref: args_ref,
                    execution_target: Some(HostToolTargets::local_execution_target()),
                }],
            },
        )
        .await
        .expect("invoke batch")
        .single_result()
        .expect("single result");

        let record = store
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        assert_eq!(result.status, ToolCallStatus::Succeeded);
        assert_eq!(result.effects.len(), 1);
        assert_eq!(result.effects[0].kind, VFS_WORKSPACE_COMMIT_EFFECT_KIND);
        assert_eq!(
            result.effects[0]
                .data
                .get("workspace_id")
                .map(String::as_str),
            Some(workspace_id.as_str())
        );
        assert_eq!(
            result.effects[0]
                .data
                .get("snapshot_ref")
                .map(String::as_str),
            Some(record.head_snapshot_ref.as_str())
        );
        assert_eq!(
            result.effects[0].data.get("revision").map(String::as_str),
            Some("1")
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

    #[tokio::test(flavor = "current_thread")]
    async fn mounted_vfs_file_system_lists_synthetic_directories_and_routes_mounts() {
        let (_blobs, _store, fs, workspace_id) = test_mounted_fs().await;

        assert_eq!(fs.access_policy(), FileAccessPolicy::FullReadWrite);
        assert_eq!(
            fs.read_directory(&FsPath::root()).await.expect("list root"),
            vec![
                ReadDirectoryEntry {
                    file_name: "skills".to_owned(),
                    is_directory: true,
                    is_file: false,
                },
                ReadDirectoryEntry {
                    file_name: "workspace".to_owned(),
                    is_directory: true,
                    is_file: false,
                },
            ]
        );
        assert_eq!(
            fs.read_directory(&FsPath::new("/skills").unwrap())
                .await
                .expect("list synthetic skills"),
            vec![ReadDirectoryEntry {
                file_name: "rust".to_owned(),
                is_directory: true,
                is_file: false,
            }]
        );
        assert!(
            fs.get_metadata(&FsPath::new("/skills").unwrap())
                .await
                .expect("stat synthetic skills")
                .is_directory
        );
        assert_eq!(
            fs.read_file_text(&FsPath::new("/skills/rust/SKILL.md").unwrap())
                .await
                .expect("read skill"),
            "# Rust Skill\n"
        );

        assert!(matches!(
            fs.write_file(
                &FsPath::new("/skills/rust/SKILL.md").unwrap(),
                b"updated".to_vec()
            )
            .await,
            Err(FsError::PermissionDenied { .. })
        ));
        fs.write_file(
            &FsPath::new("/workspace/out.txt").unwrap(),
            b"out\n".to_vec(),
        )
        .await
        .expect("write workspace");
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/out.txt").unwrap())
                .await
                .expect("read workspace file"),
            "out\n"
        );

        let record = fs
            .workspace_store
            .read_workspace(&workspace_id)
            .await
            .expect("read workspace");
        assert_eq!(record.revision, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mounted_vfs_file_system_rejects_invalid_mount_tables() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let store = Arc::new(TestWorkspaceStore::default());
        let session_id = SessionId::new("session_1");
        let snapshot_ref = BlobRef::from_bytes(b"snapshot");
        let workspace_id = ::vfs::VfsWorkspaceId::new("workspace_1");

        let duplicate = vec![
            mount_record(
                &session_id,
                "/workspace",
                ::vfs::VfsMountSource::Workspace {
                    workspace_id: workspace_id.clone(),
                },
                ::vfs::VfsMountAccess::ReadWrite,
            ),
            mount_record(
                &session_id,
                "/workspace",
                ::vfs::VfsMountSource::Workspace {
                    workspace_id: workspace_id.clone(),
                },
                ::vfs::VfsMountAccess::ReadWrite,
            ),
        ];
        assert!(matches!(
            MountedVfsFileSystem::new(blobs.clone(), store.clone(), duplicate),
            Err(FsError::InvalidInput { .. })
        ));

        let nested = vec![
            mount_record(
                &session_id,
                "/skills",
                ::vfs::VfsMountSource::Snapshot {
                    snapshot_ref: snapshot_ref.clone(),
                },
                ::vfs::VfsMountAccess::ReadOnly,
            ),
            mount_record(
                &session_id,
                "/skills/rust",
                ::vfs::VfsMountSource::Snapshot {
                    snapshot_ref: snapshot_ref.clone(),
                },
                ::vfs::VfsMountAccess::ReadOnly,
            ),
        ];
        assert!(matches!(
            MountedVfsFileSystem::new(blobs.clone(), store.clone(), nested),
            Err(FsError::InvalidInput { .. })
        ));

        let writable_snapshot = vec![mount_record(
            &session_id,
            "/skills/rust",
            ::vfs::VfsMountSource::Snapshot { snapshot_ref },
            ::vfs::VfsMountAccess::ReadWrite,
        )];
        assert!(matches!(
            MountedVfsFileSystem::new(blobs, store, writable_snapshot),
            Err(FsError::InvalidInput { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mounted_vfs_file_system_copies_across_mounts() {
        let (_blobs, _store, fs, _workspace_id) = test_mounted_fs().await;

        fs.copy(
            &FsPath::new("/skills/rust/SKILL.md").unwrap(),
            &FsPath::new("/workspace/SKILL-copy.md").unwrap(),
            CopyOptions::file(),
        )
        .await
        .expect("copy file across mounts");
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/SKILL-copy.md").unwrap())
                .await
                .expect("read copied skill"),
            "# Rust Skill\n"
        );

        fs.copy(
            &FsPath::new("/skills/rust").unwrap(),
            &FsPath::new("/workspace/skill-copy").unwrap(),
            CopyOptions::recursive(),
        )
        .await
        .expect("copy directory across mounts");
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/skill-copy/references/info.md").unwrap())
                .await
                .expect("read copied reference"),
            "reference\n"
        );

        assert!(matches!(
            fs.copy(
                &FsPath::new("/workspace/SKILL-copy.md").unwrap(),
                &FsPath::new("/skills/rust/generated.md").unwrap(),
                CopyOptions::file(),
            )
            .await,
            Err(FsError::PermissionDenied { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn existing_file_tools_work_against_mounted_vfs_file_system() {
        let (blobs, _store, fs, _workspace_id) = test_mounted_fs().await;
        let ctx = HostToolContext::new(Arc::new(fs.clone()), None, blobs)
            .with_cwd(FsPath::new("/workspace").unwrap());

        let skill = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/skills/rust/SKILL.md").unwrap(),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read skill through mounted fs");
        assert_eq!(skill.text, "# Rust Skill");

        invoke_write_file(
            &ctx,
            WriteFileArgs {
                path: FsPath::new("src/lib.rs").unwrap(),
                content: "pub fn alpha() {}\n".to_owned(),
            },
        )
        .await
        .expect("write workspace through mounted fs");
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
        .expect("edit workspace through mounted fs");
        invoke_apply_patch(
            &ctx,
            ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: notes.md\n+mounted\n*** End Patch"
                    .to_owned(),
            },
        )
        .await
        .expect("patch workspace through mounted fs");

        let grep = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "beta".to_owned(),
                path: Some(FsPath::root()),
                include: Some("*.rs".to_owned()),
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep mounted fs");
        assert_eq!(
            grep.matches
                .iter()
                .map(|match_| match_.path.clone())
                .collect::<Vec<_>>(),
            vec![FsPath::new("/workspace/src/lib.rs").unwrap()]
        );

        let glob = invoke_glob(
            &ctx,
            GlobArgs {
                pattern: "**/*.md".to_owned(),
                path: Some(FsPath::root()),
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("glob mounted fs");
        assert_eq!(
            glob.matches,
            vec![
                FsPath::new("/skills/rust/SKILL.md").unwrap(),
                FsPath::new("/skills/rust/references/info.md").unwrap(),
                FsPath::new("/workspace/notes.md").unwrap(),
            ]
        );

        let listing = invoke_list_dir(
            &ctx,
            ListDirArgs {
                path: FsPath::root(),
            },
        )
        .await
        .expect("list mounted root");
        assert_eq!(listing.entries.len(), 2);
    }
}
