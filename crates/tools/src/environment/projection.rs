//! Runtime-owned environment and VFS context projection snapshots.

use engine::{
    BlobRef, ContextEntryInput, ContextEntryKey, ContextEntryKind, CoreAgentCommand,
    CoreAgentState, ENVIRONMENT_ACTIVE_CONTEXT_KEY, ENVIRONMENT_CATALOG_CONTEXT_KEY,
    ToolExecutionTarget, VFS_CATALOG_CONTEXT_KEY,
    storage::{BlobStore, BlobStoreError},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use vfs::{VfsMountAccess, VfsMountRecord, VfsMountSource};

use crate::fs::FsPath;

pub const VFS_CATALOG_SCHEMA_VERSION: &str = "lightspeed.environment.vfs_catalog.v1";
pub const ENVIRONMENT_CATALOG_SCHEMA_VERSION: &str = "lightspeed.environment.catalog.v1";
pub const ENVIRONMENT_ACTIVE_SCHEMA_VERSION: &str = "lightspeed.environment.active.v1";
pub const ENVIRONMENT_PROJECTION_MEDIA_TYPE: &str = "application/json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsCatalog {
    pub schema_version: String,
    pub revision: u64,
    pub routes: Vec<FsRoute>,
}

impl VfsCatalog {
    pub fn new(revision: u64, routes: Vec<FsRoute>) -> Self {
        Self {
            schema_version: VFS_CATALOG_SCHEMA_VERSION.to_owned(),
            revision,
            routes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentCatalogSnapshot {
    pub schema_version: String,
    pub revision: u64,
    pub active_env_id: Option<String>,
    pub environments: Vec<EnvironmentRecord>,
}

impl EnvironmentCatalogSnapshot {
    pub fn new(
        revision: u64,
        active_env_id: Option<String>,
        environments: Vec<EnvironmentRecord>,
    ) -> Self {
        Self {
            schema_version: ENVIRONMENT_CATALOG_SCHEMA_VERSION.to_owned(),
            revision,
            active_env_id,
            environments,
        }
    }

    pub fn empty(revision: u64) -> Self {
        Self::new(revision, None, Vec::new())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRecord {
    pub env_id: String,
    pub kind: EnvironmentKind,
    pub capabilities: EnvironmentCapabilities,
    pub exec_target: Option<ToolExecutionTarget>,
    pub cwd: Option<FsPath>,
    pub status: EnvironmentStatus,
    pub description_ref: Option<BlobRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    Sandbox,
    RemoteHost,
    AttachedHost,
    Connector,
    Browser,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentCapabilities {
    #[serde(default)]
    pub fs_read: bool,
    #[serde(default)]
    pub fs_write: bool,
    #[serde(default)]
    pub process_exec: bool,
    #[serde(default)]
    pub process_stdin: bool,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub persistent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    Attaching,
    Ready,
    Degraded,
    Detached,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentActive {
    pub schema_version: String,
    pub revision: u64,
    pub env_id: String,
    pub fs_routes: Vec<FsRoute>,
}

impl EnvironmentActive {
    pub fn new(revision: u64, env_id: impl Into<String>, fs_routes: Vec<FsRoute>) -> Self {
        Self {
            schema_version: ENVIRONMENT_ACTIVE_SCHEMA_VERSION.to_owned(),
            revision,
            env_id: env_id.into(),
            fs_routes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRoute {
    pub path: FsPath,
    pub access: FsRouteAccess,
    pub source: FsRouteSource,
    pub same_state_as_active_env: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsRouteAccess {
    ReadOnly,
    ReadWrite,
}

impl From<VfsMountAccess> for FsRouteAccess {
    fn from(value: VfsMountAccess) -> Self {
        match value {
            VfsMountAccess::ReadOnly => Self::ReadOnly,
            VfsMountAccess::ReadWrite => Self::ReadWrite,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FsRouteSource {
    VfsSnapshot { snapshot_ref: BlobRef },
    VfsWorkspace { workspace_id: String },
    HostFilesystem { target: ToolExecutionTarget },
    FusedWorkspace { env_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProjectionPublication<T> {
    pub snapshot_ref: BlobRef,
    pub snapshot: T,
    pub snapshot_bytes: Vec<u8>,
    pub command: Option<CoreAgentCommand>,
}

#[derive(Debug, Error)]
pub enum EnvironmentProjectionError {
    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("failed to encode environment projection: {message}")]
    Encode { message: String },

    #[error("invalid environment projection path {path}: {message}")]
    InvalidPath { path: String, message: String },
}

pub async fn prepare_vfs_catalog_publication(
    blobs: &dyn BlobStore,
    state: &CoreAgentState,
    catalog: VfsCatalog,
) -> Result<EnvironmentProjectionPublication<VfsCatalog>, EnvironmentProjectionError> {
    prepare_projection_publication(
        blobs,
        state,
        catalog,
        VFS_CATALOG_CONTEXT_KEY,
        vfs_catalog_context_input,
    )
    .await
}

pub async fn prepare_environment_catalog_publication(
    blobs: &dyn BlobStore,
    state: &CoreAgentState,
    catalog: EnvironmentCatalogSnapshot,
) -> Result<EnvironmentProjectionPublication<EnvironmentCatalogSnapshot>, EnvironmentProjectionError>
{
    prepare_projection_publication(
        blobs,
        state,
        catalog,
        ENVIRONMENT_CATALOG_CONTEXT_KEY,
        environment_catalog_context_input,
    )
    .await
}

pub async fn prepare_environment_active_publication(
    blobs: &dyn BlobStore,
    state: &CoreAgentState,
    active: EnvironmentActive,
) -> Result<EnvironmentProjectionPublication<EnvironmentActive>, EnvironmentProjectionError> {
    prepare_projection_publication(
        blobs,
        state,
        active,
        ENVIRONMENT_ACTIVE_CONTEXT_KEY,
        environment_active_context_input,
    )
    .await
}

pub fn vfs_catalog_from_mounts(
    mounts: &[VfsMountRecord],
) -> Result<VfsCatalog, EnvironmentProjectionError> {
    let mut routes = mounts
        .iter()
        .map(fs_route_from_vfs_mount)
        .collect::<Result<Vec<_>, _>>()?;
    routes.sort_by(|left, right| left.path.cmp(&right.path));
    let revision = stable_revision(&encode_json(&routes)?);
    Ok(VfsCatalog::new(revision, routes))
}

pub fn empty_environment_catalog(revision: u64) -> EnvironmentCatalogSnapshot {
    EnvironmentCatalogSnapshot::empty(revision)
}

pub fn vfs_catalog_context_input(catalog_ref: BlobRef) -> ContextEntryInput {
    projection_context_input(ContextEntryKind::VfsCatalog, catalog_ref, "VFS catalog")
}

pub fn environment_catalog_context_input(catalog_ref: BlobRef) -> ContextEntryInput {
    projection_context_input(
        ContextEntryKind::EnvironmentCatalog,
        catalog_ref,
        "environment catalog",
    )
}

pub fn environment_active_context_input(active_ref: BlobRef) -> ContextEntryInput {
    projection_context_input(
        ContextEntryKind::EnvironmentActive,
        active_ref,
        "active environment",
    )
}

pub fn current_vfs_catalog_ref(state: &CoreAgentState) -> Option<BlobRef> {
    current_context_ref(state, VFS_CATALOG_CONTEXT_KEY, ContextEntryKind::VfsCatalog)
}

pub fn current_environment_catalog_ref(state: &CoreAgentState) -> Option<BlobRef> {
    current_context_ref(
        state,
        ENVIRONMENT_CATALOG_CONTEXT_KEY,
        ContextEntryKind::EnvironmentCatalog,
    )
}

pub fn current_environment_active_ref(state: &CoreAgentState) -> Option<BlobRef> {
    current_context_ref(
        state,
        ENVIRONMENT_ACTIVE_CONTEXT_KEY,
        ContextEntryKind::EnvironmentActive,
    )
}

pub fn clear_environment_active_command(active_ref: Option<&BlobRef>) -> Option<CoreAgentCommand> {
    active_ref.map(|_| CoreAgentCommand::RemoveContext {
        key: ContextEntryKey::new(ENVIRONMENT_ACTIVE_CONTEXT_KEY),
    })
}

async fn prepare_projection_publication<T>(
    blobs: &dyn BlobStore,
    state: &CoreAgentState,
    snapshot: T,
    key: &'static str,
    context_input: fn(BlobRef) -> ContextEntryInput,
) -> Result<EnvironmentProjectionPublication<T>, EnvironmentProjectionError>
where
    T: Clone + PartialEq + Serialize,
{
    let snapshot_bytes = encode_json(&snapshot)?;
    let snapshot_ref = blobs.put_bytes(snapshot_bytes.clone()).await?;
    let command = if current_key_ref(state, key).as_ref() == Some(&snapshot_ref) {
        None
    } else {
        Some(CoreAgentCommand::UpsertContext {
            key: ContextEntryKey::new(key),
            entry: context_input(snapshot_ref.clone()),
        })
    };

    Ok(EnvironmentProjectionPublication {
        snapshot_ref,
        snapshot,
        snapshot_bytes,
        command,
    })
}

fn fs_route_from_vfs_mount(record: &VfsMountRecord) -> Result<FsRoute, EnvironmentProjectionError> {
    let path = FsPath::new(record.mount_path.as_str()).map_err(|error| {
        EnvironmentProjectionError::InvalidPath {
            path: record.mount_path.as_str().to_owned(),
            message: error.to_string(),
        }
    })?;
    let source = match &record.source {
        VfsMountSource::Snapshot { snapshot_ref } => FsRouteSource::VfsSnapshot {
            snapshot_ref: snapshot_ref.clone(),
        },
        VfsMountSource::Workspace { workspace_id } => FsRouteSource::VfsWorkspace {
            workspace_id: workspace_id.as_str().to_owned(),
        },
    };
    Ok(FsRoute {
        path,
        access: record.access.into(),
        source,
        same_state_as_active_env: None,
    })
}

fn projection_context_input(
    kind: ContextEntryKind,
    content_ref: BlobRef,
    preview: &'static str,
) -> ContextEntryInput {
    ContextEntryInput {
        kind,
        content_ref,
        media_type: Some(ENVIRONMENT_PROJECTION_MEDIA_TYPE.to_owned()),
        preview: Some(preview.to_owned()),
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

fn current_context_ref(
    state: &CoreAgentState,
    key: &'static str,
    kind: ContextEntryKind,
) -> Option<BlobRef> {
    state
        .context
        .entries
        .iter()
        .find(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|entry_key| entry_key.as_str() == key)
                && entry.kind == kind
        })
        .map(|entry| entry.content_ref.clone())
}

fn current_key_ref(state: &CoreAgentState, key: &'static str) -> Option<BlobRef> {
    state
        .context
        .entries
        .iter()
        .find(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|entry_key| entry_key.as_str() == key)
        })
        .map(|entry| entry.content_ref.clone())
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, EnvironmentProjectionError> {
    serde_json::to_vec(value).map_err(|error| EnvironmentProjectionError::Encode {
        message: error.to_string(),
    })
}

fn stable_revision(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use engine::{SessionId, storage::InMemoryBlobStore};
    use vfs::{VfsMountAccess, VfsMountSource, VfsPath, VfsWorkspaceId};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_catalog_publication_skips_unchanged_catalog() {
        let blobs = InMemoryBlobStore::new();
        let catalog = VfsCatalog::new(0, Vec::new());
        let state = CoreAgentState::new();

        let first = prepare_vfs_catalog_publication(&blobs, &state, catalog.clone())
            .await
            .expect("first publication");
        assert!(first.command.is_some());

        let mut state = CoreAgentState::new();
        state.context.entries = vec![engine::ContextEntry {
            entry_id: engine::ContextEntryId::new(1),
            key: Some(ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY)),
            kind: ContextEntryKind::VfsCatalog,
            source: engine::ContextEntrySource::Runtime {
                label: "environment.projection".to_owned(),
            },
            content_ref: first.snapshot_ref.clone(),
            media_type: Some(ENVIRONMENT_PROJECTION_MEDIA_TYPE.to_owned()),
            preview: Some("VFS catalog".to_owned()),
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }];

        let second = prepare_vfs_catalog_publication(&blobs, &state, catalog)
            .await
            .expect("second publication");
        assert!(second.command.is_none());
    }

    #[test]
    fn vfs_catalog_from_mounts_projects_routes() {
        let mount = VfsMountRecord {
            session_id: SessionId::new("session_1"),
            mount_path: VfsPath::parse("/workspace").expect("mount path"),
            source: VfsMountSource::Workspace {
                workspace_id: VfsWorkspaceId::new("workspace_1"),
            },
            access: VfsMountAccess::ReadWrite,
        };

        let catalog = vfs_catalog_from_mounts(&[mount]).expect("catalog");

        assert_ne!(catalog.revision, 0);
        assert_eq!(catalog.routes.len(), 1);
        assert_eq!(catalog.routes[0].path.as_str(), "/workspace");
        assert_eq!(catalog.routes[0].access, FsRouteAccess::ReadWrite);
        assert_eq!(catalog.routes[0].same_state_as_active_env, None);
        assert!(matches!(
            catalog.routes[0].source,
            FsRouteSource::VfsWorkspace { .. }
        ));
    }
}
