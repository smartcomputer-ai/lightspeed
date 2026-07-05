use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use engine::{BlobRef, SessionId};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{fs, sync::Mutex};

use ::vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountRecord,
    VfsMountSource, VfsMountStore, VfsPath, VfsSnapshotRecord, VfsSnapshotStore, VfsWorkspaceId,
    VfsWorkspaceRecord, VfsWorkspaceStore,
};

#[derive(Clone)]
pub struct FsVfsCatalogStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl FsVfsCatalogStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self::new(root);
        store.ensure_layout().await?;
        Ok(store)
    }

    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(crate::lightspeed_dir(project_root))
    }

    pub async fn open_project(project_root: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(crate::lightspeed_dir(project_root)).await
    }

    pub fn root(&self) -> &Path {
        self.root.as_ref().as_path()
    }

    async fn ensure_layout(&self) -> io::Result<()> {
        fs::create_dir_all(self.snapshots_root()).await?;
        fs::create_dir_all(self.workspaces_root()).await?;
        fs::create_dir_all(self.mounts_root()).await?;
        Ok(())
    }

    fn vfs_root(&self) -> PathBuf {
        crate::vfs_dir(self.root())
    }

    fn snapshots_root(&self) -> PathBuf {
        self.vfs_root().join("snapshots")
    }

    fn workspaces_root(&self) -> PathBuf {
        self.vfs_root().join("workspaces")
    }

    fn mounts_root(&self) -> PathBuf {
        self.vfs_root().join("mounts")
    }

    fn snapshot_path(&self, snapshot_ref: &BlobRef) -> PathBuf {
        self.snapshots_root().join(format!(
            "{}.json",
            crate::encode_component(snapshot_ref.as_str())
        ))
    }

    fn workspace_path(&self, workspace_id: &VfsWorkspaceId) -> PathBuf {
        self.workspaces_root().join(format!(
            "{}.json",
            crate::encode_component(workspace_id.as_str())
        ))
    }

    fn session_mounts_dir(&self, session_id: &SessionId) -> PathBuf {
        self.mounts_root()
            .join(crate::encode_component(session_id.as_str()))
    }

    fn mount_path(&self, session_id: &SessionId, mount_path: &VfsPath) -> PathBuf {
        self.session_mounts_dir(session_id).join(format!(
            "{}.json",
            crate::encode_component(mount_path.as_str())
        ))
    }

    async fn remove_workspace_mounts_locked(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<(), VfsCatalogError> {
        let root = self.mounts_root();
        let mut sessions = match fs::read_dir(&root).await {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(catalog_io_error("read vfs mounts directory", &root, error)),
        };

        while let Some(session_entry) = sessions
            .next_entry()
            .await
            .map_err(|error| catalog_io_error("read vfs mounts directory", &root, error))?
        {
            let session_dir = session_entry.path();
            let file_type = session_entry.file_type().await.map_err(|error| {
                catalog_io_error("stat vfs session mounts directory", &session_dir, error)
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let mut mounts = match fs::read_dir(&session_dir).await {
                Ok(read_dir) => read_dir,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(catalog_io_error(
                        "read vfs session mounts directory",
                        &session_dir,
                        error,
                    ));
                }
            };
            while let Some(mount_entry) = mounts.next_entry().await.map_err(|error| {
                catalog_io_error("read vfs session mounts directory", &session_dir, error)
            })? {
                let path = mount_entry.path();
                let file_type = mount_entry
                    .file_type()
                    .await
                    .map_err(|error| catalog_io_error("stat vfs mount record", &path, error))?;
                if !file_type.is_file() {
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }

                let mount: VfsMountRecord =
                    read_required_json("mount", "unknown", "read vfs mount record", &path).await?;
                if matches!(
                    &mount.source,
                    VfsMountSource::Workspace {
                        workspace_id: mounted_workspace_id
                    } if mounted_workspace_id == workspace_id
                ) {
                    fs::remove_file(&path).await.map_err(|error| {
                        catalog_io_error("delete vfs mount record", &path, error)
                    })?;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl VfsSnapshotStore for FsVfsCatalogStore {
    async fn record_snapshot(&self, record: VfsSnapshotRecord) -> Result<(), VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.snapshot_path(&record.snapshot_ref);
        write_json("write vfs snapshot record", &path, &record).await
    }

    async fn read_snapshot(
        &self,
        snapshot_ref: &BlobRef,
    ) -> Result<VfsSnapshotRecord, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.snapshot_path(snapshot_ref);
        read_required_json(
            "snapshot",
            snapshot_ref.as_str(),
            "read vfs snapshot record",
            &path,
        )
        .await
    }
}

#[async_trait]
impl VfsWorkspaceStore for FsVfsCatalogStore {
    async fn create_workspace(
        &self,
        record: CreateVfsWorkspaceRecord,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.workspace_path(&record.workspace_id);
        if crate::path_exists(&path)
            .await
            .map_err(|error| catalog_io_error("stat vfs workspace record", &path, error))?
        {
            return Err(VfsCatalogError::AlreadyExists {
                kind: "workspace",
                id: record.workspace_id.to_string(),
            });
        }

        let workspace = VfsWorkspaceRecord {
            workspace_id: record.workspace_id,
            display_name: record.display_name,
            base_snapshot_ref: record.base_snapshot_ref,
            head_snapshot_ref: record.head_snapshot_ref,
            head_totals: record.head_totals,
            revision: 0,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.created_at_ms,
        };
        write_json("write vfs workspace record", &path, &workspace).await?;
        Ok(workspace)
    }

    async fn read_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.workspace_path(workspace_id);
        read_required_json(
            "workspace",
            workspace_id.as_str(),
            "read vfs workspace record",
            &path,
        )
        .await
    }

    async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let dir = self.workspaces_root();
        let mut read_dir = match fs::read_dir(&dir).await {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(catalog_io_error("read vfs workspaces directory", &dir, error));
            }
        };

        let mut workspaces = Vec::new();
        while let Some(entry) = read_dir.next_entry().await.map_err(|error| {
            catalog_io_error("read vfs workspaces directory entry", &dir, error)
        })? {
            let file_type = entry.file_type().await.map_err(|error| {
                catalog_io_error("read vfs workspace entry type", &entry.path(), error)
            })?;
            if !file_type.is_file() {
                continue;
            }
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("json")
            {
                continue;
            }
            workspaces.push(
                read_required_json::<VfsWorkspaceRecord>(
                    "workspace",
                    &entry.path().display().to_string(),
                    "read vfs workspace record",
                    &entry.path(),
                )
                .await?,
            );
        }
        workspaces.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.workspace_id.cmp(&right.workspace_id))
        });
        Ok(workspaces)
    }

    async fn compare_and_set_head(
        &self,
        request: CompareAndSetVfsWorkspaceHead,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.workspace_path(&request.workspace_id);
        let mut workspace: VfsWorkspaceRecord = read_required_json(
            "workspace",
            request.workspace_id.as_str(),
            "read vfs workspace record",
            &path,
        )
        .await?;
        if let Some(expected_revision) = request.expected_revision
            && workspace.revision != expected_revision
        {
            return Err(VfsCatalogError::RevisionConflict {
                workspace_id: request.workspace_id,
                expected_revision,
                actual_revision: workspace.revision,
            });
        }

        if let Some(display_name) = request.display_name {
            workspace.display_name = Some(display_name);
        }
        workspace.head_snapshot_ref = request.new_head_snapshot_ref;
        workspace.head_totals = request.new_head_totals;
        workspace.revision =
            workspace
                .revision
                .checked_add(1)
                .ok_or_else(|| VfsCatalogError::Store {
                    message: format!(
                        "vfs workspace revision overflow for {}",
                        workspace.workspace_id
                    ),
                })?;
        workspace.updated_at_ms = request.updated_at_ms;
        write_json("write vfs workspace record", &path, &workspace).await?;
        Ok(workspace)
    }

    async fn delete_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.workspace_path(workspace_id);
        let workspace: VfsWorkspaceRecord = read_required_json(
            "workspace",
            workspace_id.as_str(),
            "read vfs workspace record",
            &path,
        )
        .await?;
        fs::remove_file(&path)
            .await
            .map_err(|error| catalog_io_error("delete vfs workspace record", &path, error))?;
        self.remove_workspace_mounts_locked(workspace_id).await?;
        Ok(workspace)
    }
}

#[async_trait]
impl VfsMountStore for FsVfsCatalogStore {
    async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.mount_path(&record.session_id, &record.mount_path);
        write_json("write vfs mount record", &path, &record).await
    }

    async fn list_mounts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let dir = self.session_mounts_dir(session_id);
        let mut read_dir = match fs::read_dir(&dir).await {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(catalog_io_error("read vfs mounts directory", &dir, error)),
        };

        let mut mounts = Vec::new();
        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|error| catalog_io_error("read vfs mounts directory entry", &dir, error))?
        {
            let file_type = entry.file_type().await.map_err(|error| {
                catalog_io_error("read vfs mount entry type", &entry.path(), error)
            })?;
            if !file_type.is_file() {
                continue;
            }
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("json")
            {
                continue;
            }
            mounts.push(
                read_required_json::<VfsMountRecord>(
                    "mount",
                    &format!("{}:{}", session_id, entry.path().display()),
                    "read vfs mount record",
                    &entry.path(),
                )
                .await?,
            );
        }
        mounts.sort_by(|left, right| left.mount_path.cmp(&right.mount_path));
        Ok(mounts)
    }

    async fn remove_mount(
        &self,
        session_id: &SessionId,
        mount_path: &VfsPath,
    ) -> Result<(), VfsCatalogError> {
        let _guard = self.lock.lock().await;
        let path = self.mount_path(session_id, mount_path);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(VfsCatalogError::NotFound {
                    kind: "mount",
                    id: format!("{session_id}:{mount_path}"),
                })
            }
            Err(error) => Err(catalog_io_error("remove vfs mount record", &path, error)),
        }
    }
}

async fn read_required_json<T: DeserializeOwned>(
    kind: &'static str,
    id: &str,
    action: &str,
    path: &Path,
) -> Result<T, VfsCatalogError> {
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(VfsCatalogError::NotFound {
                kind,
                id: id.to_string(),
            });
        }
        Err(error) => return Err(catalog_io_error(action, path, error)),
    };
    serde_json::from_slice(&bytes).map_err(|error| VfsCatalogError::Store {
        message: format!("{action} '{}': decode JSON: {error}", path.display()),
    })
}

async fn write_json<T: Serialize>(
    action: &str,
    path: &Path,
    value: &T,
) -> Result<(), VfsCatalogError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| VfsCatalogError::Store {
        message: format!("{action} '{}': encode JSON: {error}", path.display()),
    })?;
    bytes.push(b'\n');
    crate::atomic_write(path, &bytes)
        .await
        .map_err(|error| catalog_io_error(action, path, error))
}

fn catalog_io_error(action: &str, path: &Path, error: io::Error) -> VfsCatalogError {
    VfsCatalogError::Store {
        message: format!("{action} '{}': {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::vfs::{
        VfsMountAccess, VfsMountSource, VfsSnapshotSource, VfsSnapshotSource as SnapshotSource,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn fs_vfs_catalog_persists_snapshot_workspace_and_mounts() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsVfsCatalogStore::open(temp_dir.path())
            .await
            .expect("open vfs store");

        let snapshot_ref = BlobRef::from_bytes(b"snapshot");
        let record = VfsSnapshotRecord {
            snapshot_ref: snapshot_ref.clone(),
            source: SnapshotSource::new("inline").with_subject("seed"),
            display_name: Some("Seed".to_string()),
            created_at_ms: 1,
        };
        store
            .record_snapshot(record.clone())
            .await
            .expect("record snapshot");
        assert_eq!(
            store
                .read_snapshot(&snapshot_ref)
                .await
                .expect("read snapshot"),
            record
        );

        let workspace_id = VfsWorkspaceId::new("workspace-1");
        let workspace = store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                display_name: Some("Scratch".to_string()),
                base_snapshot_ref: Some(snapshot_ref.clone()),
                head_snapshot_ref: snapshot_ref.clone(),
                head_totals: ::vfs::VfsTotals { files: 1, bytes: 8 },
                created_at_ms: 2,
            })
            .await
            .expect("create workspace");
        assert_eq!(workspace.revision, 0);
        assert_eq!(workspace.display_name.as_deref(), Some("Scratch"));
        assert_eq!(workspace.head_totals, ::vfs::VfsTotals { files: 1, bytes: 8 });
        assert!(matches!(
            store
                .create_workspace(CreateVfsWorkspaceRecord {
                    workspace_id: workspace_id.clone(),
                    display_name: None,
                    base_snapshot_ref: None,
                    head_snapshot_ref: snapshot_ref.clone(),
                    head_totals: ::vfs::VfsTotals::default(),
                    created_at_ms: 3,
                })
                .await,
            Err(VfsCatalogError::AlreadyExists { .. })
        ));

        let next_ref = BlobRef::from_bytes(b"next");
        let updated = store
            .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                workspace_id: workspace_id.clone(),
                expected_revision: Some(0),
                display_name: Some("Scratch v2".to_string()),
                new_head_snapshot_ref: next_ref.clone(),
                new_head_totals: ::vfs::VfsTotals { files: 2, bytes: 16 },
                updated_at_ms: 4,
            })
            .await
            .expect("advance head");
        assert_eq!(updated.revision, 1);
        assert_eq!(updated.head_snapshot_ref, next_ref);
        assert_eq!(updated.display_name.as_deref(), Some("Scratch v2"));
        assert_eq!(updated.head_totals, ::vfs::VfsTotals { files: 2, bytes: 16 });
        assert!(matches!(
            store
                .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                    workspace_id: workspace_id.clone(),
                    expected_revision: Some(0),
                    display_name: None,
                    new_head_snapshot_ref: BlobRef::from_bytes(b"stale"),
                    new_head_totals: ::vfs::VfsTotals::default(),
                    updated_at_ms: 5,
                })
                .await,
            Err(VfsCatalogError::RevisionConflict {
                actual_revision: 1,
                ..
            })
        ));
        let listed = store.list_workspaces().await.expect("list workspaces");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].workspace_id, workspace_id);
        assert_eq!(listed[0].display_name.as_deref(), Some("Scratch v2"));

        let session_id = SessionId::new("session-1");
        let skill_mount = VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse("/skills/openai-docs").unwrap(),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        };
        let workspace_mount = VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse("/workspace").unwrap(),
            source: VfsMountSource::Workspace {
                workspace_id: workspace_id.clone(),
            },
            access: VfsMountAccess::ReadWrite,
        };
        store
            .put_mount(workspace_mount.clone())
            .await
            .expect("put workspace mount");
        store
            .put_mount(skill_mount.clone())
            .await
            .expect("put skill mount");
        assert_eq!(
            store.list_mounts(&session_id).await.expect("list mounts"),
            vec![skill_mount.clone(), workspace_mount.clone()]
        );
        store
            .remove_mount(&session_id, &skill_mount.mount_path)
            .await
            .expect("remove mount");
        assert_eq!(
            store.list_mounts(&session_id).await.expect("list mounts"),
            vec![workspace_mount.clone()]
        );
        let deleted = store
            .delete_workspace(&workspace_id)
            .await
            .expect("delete workspace");
        assert_eq!(deleted.workspace_id, workspace_id);
        assert!(matches!(
            store.read_workspace(&deleted.workspace_id).await,
            Err(VfsCatalogError::NotFound { .. })
        ));
        assert_eq!(
            store.list_mounts(&session_id).await.expect("list mounts"),
            Vec::<VfsMountRecord>::new()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_store_opens_vfs_catalog() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = crate::FsStore::open(temp_dir.path())
            .await
            .expect("open fs store");

        let snapshot_ref = BlobRef::from_bytes(b"snapshot");
        let record = VfsSnapshotRecord {
            snapshot_ref: snapshot_ref.clone(),
            source: VfsSnapshotSource::unknown(),
            display_name: None,
            created_at_ms: 1,
        };
        store
            .vfs()
            .record_snapshot(record.clone())
            .await
            .expect("record snapshot");
        assert_eq!(
            store
                .vfs()
                .read_snapshot(&snapshot_ref)
                .await
                .expect("read snapshot"),
            record
        );
    }
}
