//! Runtime-owned environment and VFS context projection snapshots.

use std::collections::BTreeSet;

use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand, WorkspaceAccess, WorkspaceAttachmentTarget,
    storage::{BlobGraphStore, BlobStore, BlobStoreError, record_contains_edges},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use vfs::{ResolvedWorkspaceAttachment, ResolvedWorkspaceAttachmentTarget};

use crate::catalog::{VFS_CATALOG_CONTEXT_KEY, catalog_context_input, catalog_publication_command};
use crate::fs::FsPath;

pub const VFS_CATALOG_SCHEMA_VERSION: &str = "lightspeed.environment.vfs_catalog.v1";

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
pub struct FsRoute {
    pub path: FsPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<FsPath>,
    pub access: FsRouteAccess,
    pub source: FsRouteSource,
    pub availability: FsRouteAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FsRouteAvailability {
    Available,
    Unavailable { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsRouteAccess {
    ReadOnly,
    ReadWrite,
}

impl From<WorkspaceAccess> for FsRouteAccess {
    fn from(value: WorkspaceAccess) -> Self {
        match value {
            WorkspaceAccess::Read => Self::ReadOnly,
            WorkspaceAccess::Edit => Self::ReadWrite,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FsRouteSource {
    VfsSnapshot { snapshot_ref: BlobRef },
    VfsWorkspace { workspace_id: String },
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
    blob_graph: Option<&dyn BlobGraphStore>,
    current: Option<&ContextEntryInput>,
    catalog: VfsCatalog,
) -> Result<EnvironmentProjectionPublication<VfsCatalog>, EnvironmentProjectionError> {
    let snapshot_bytes = encode_json(&catalog)?;
    let snapshot_ref = blobs.put_bytes(snapshot_bytes.clone()).await?;
    record_contains_edges(blob_graph, &snapshot_ref, vfs_catalog_blob_refs(&catalog)).await?;
    let entry = vfs_catalog_context_input(blobs, &catalog, snapshot_ref.clone()).await?;
    let command = catalog_publication_command(current, VFS_CATALOG_CONTEXT_KEY, entry);
    Ok(EnvironmentProjectionPublication {
        snapshot_ref,
        snapshot: catalog,
        snapshot_bytes,
        command,
    })
}

/// Every snapshot manifest a catalog routes to. The catalog write records one
/// `contains` edge per ref so linked snapshots stay reachable while the
/// catalog is.
pub fn vfs_catalog_blob_refs(catalog: &VfsCatalog) -> BTreeSet<BlobRef> {
    catalog
        .routes
        .iter()
        .filter_map(|route| match &route.source {
            FsRouteSource::VfsSnapshot { snapshot_ref } => Some(snapshot_ref.clone()),
            FsRouteSource::VfsWorkspace { .. } => None,
        })
        .collect()
}

pub fn vfs_catalog_from_workspace_attachments(
    attachments: &[ResolvedWorkspaceAttachment],
) -> Result<VfsCatalog, EnvironmentProjectionError> {
    let mut routes = attachments
        .iter()
        .map(fs_route_from_workspace_attachment)
        .collect::<Result<Vec<_>, _>>()?;
    routes.sort_by(|left, right| left.path.cmp(&right.path));
    let revision = stable_revision(&encode_json(&routes)?);
    Ok(VfsCatalog::new(revision, routes))
}

pub async fn vfs_catalog_context_input(
    blobs: &dyn BlobStore,
    catalog: &VfsCatalog,
    snapshot_ref: BlobRef,
) -> Result<ContextEntryInput, BlobStoreError> {
    catalog_context_input(
        blobs,
        "Virtual filesystem (VFS)",
        super::catalog_text::vfs_catalog_text(catalog),
        snapshot_ref,
    )
    .await
}

fn fs_route_from_workspace_attachment(
    attachment: &ResolvedWorkspaceAttachment,
) -> Result<FsRoute, EnvironmentProjectionError> {
    let path = FsPath::new(attachment.path.as_str()).map_err(|error| {
        EnvironmentProjectionError::InvalidPath {
            path: attachment.path.as_str().to_owned(),
            message: error.to_string(),
        }
    })?;
    let (source, availability) = match &attachment.target {
        ResolvedWorkspaceAttachmentTarget::AvailableSnapshot { snapshot_ref } => (
            FsRouteSource::VfsSnapshot {
                snapshot_ref: snapshot_ref.clone(),
            },
            FsRouteAvailability::Available,
        ),
        ResolvedWorkspaceAttachmentTarget::AvailableWorkspace { workspace } => (
            FsRouteSource::VfsWorkspace {
                workspace_id: workspace.workspace_id.as_str().to_owned(),
            },
            FsRouteAvailability::Available,
        ),
        ResolvedWorkspaceAttachmentTarget::Unavailable {
            declared_target,
            reason,
        } => {
            let source = match declared_target {
                WorkspaceAttachmentTarget::Snapshot { snapshot_ref } => {
                    FsRouteSource::VfsSnapshot {
                        snapshot_ref: BlobRef::parse(snapshot_ref.clone()).map_err(|error| {
                            EnvironmentProjectionError::Encode {
                                message: error.to_string(),
                            }
                        })?,
                    }
                }
                WorkspaceAttachmentTarget::Workspace { workspace_id } => {
                    FsRouteSource::VfsWorkspace {
                        workspace_id: workspace_id.clone(),
                    }
                }
            };
            (
                source,
                FsRouteAvailability::Unavailable {
                    reason: reason.clone(),
                },
            )
        }
    };
    Ok(FsRoute {
        path,
        source_path: None,
        access: attachment.access.into(),
        source,
        availability,
    })
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
    use engine::{WorkspaceAccess, storage::InMemoryBlobStore};
    use vfs::{
        ResolvedWorkspaceAttachment, ResolvedWorkspaceAttachmentTarget, VfsPath, VfsWorkspaceId,
    };

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_catalog_publication_skips_unchanged_catalog() {
        let blobs = InMemoryBlobStore::new();
        let catalog = VfsCatalog::new(0, Vec::new());

        let first = prepare_vfs_catalog_publication(&blobs, None, None, catalog.clone())
            .await
            .expect("first publication");
        assert!(first.command.is_some());

        let Some(CoreAgentCommand::UpsertContext { entry, .. }) = &first.command else {
            panic!("catalog publication");
        };
        let second = prepare_vfs_catalog_publication(&blobs, None, Some(entry), catalog)
            .await
            .expect("second publication");
        assert!(second.command.is_none());
    }

    /// The recorded edge set must equal every ref the projection embeds:
    /// the snapshot manifests its routes point at.
    #[tokio::test(flavor = "current_thread")]
    async fn projection_writes_record_an_edge_for_every_embedded_ref() {
        let blobs = InMemoryBlobStore::new();
        let snapshot_ref = engine::storage::BlobStore::put_bytes(&blobs, b"snapshot".to_vec())
            .await
            .expect("put snapshot");
        let catalog = VfsCatalog::new(
            7,
            vec![
                FsRoute {
                    path: FsPath::new("/docs").expect("path"),
                    source_path: None,
                    access: FsRouteAccess::ReadOnly,
                    source: FsRouteSource::VfsSnapshot {
                        snapshot_ref: snapshot_ref.clone(),
                    },
                    availability: FsRouteAvailability::Available,
                },
                FsRoute {
                    path: FsPath::new("/workspace").expect("path"),
                    source_path: None,
                    access: FsRouteAccess::ReadWrite,
                    source: FsRouteSource::VfsWorkspace {
                        workspace_id: "workspace_1".to_owned(),
                    },
                    availability: FsRouteAvailability::Available,
                },
            ],
        );
        let publication = prepare_vfs_catalog_publication(&blobs, Some(&blobs), None, catalog)
            .await
            .expect("publication");

        let embedded = engine::storage::collect_blob_refs(
            &serde_json::from_slice(&publication.snapshot_bytes).expect("catalog json"),
        );
        let recorded: BTreeSet<BlobRef> = blobs
            .edges()
            .into_iter()
            .inspect(|edge| assert_eq!(edge.parent, publication.snapshot_ref))
            .map(|edge| edge.child)
            .collect();
        assert_eq!(recorded, embedded);
        assert_eq!(recorded, vfs_catalog_blob_refs(&publication.snapshot));
        assert_eq!(embedded, BTreeSet::from([snapshot_ref]));
    }

    #[test]
    fn vfs_catalog_from_workspace_attachments_projects_routes() {
        let attachment = workspace_attachment();

        let catalog = vfs_catalog_from_workspace_attachments(&[attachment]).expect("catalog");

        assert_ne!(catalog.revision, 0);
        assert_eq!(catalog.routes.len(), 1);
        assert_eq!(catalog.routes[0].path.as_str(), "/workspace");
        assert_eq!(catalog.routes[0].access, FsRouteAccess::ReadWrite);
        assert!(matches!(
            catalog.routes[0].source,
            FsRouteSource::VfsWorkspace { .. }
        ));
    }

    fn workspace_attachment() -> ResolvedWorkspaceAttachment {
        ResolvedWorkspaceAttachment {
            path: VfsPath::parse("/workspace").expect("attachment path"),
            target: ResolvedWorkspaceAttachmentTarget::AvailableWorkspace {
                workspace: vfs::VfsWorkspaceRecord {
                    workspace_id: VfsWorkspaceId::new("workspace_1"),
                    display_name: None,
                    base_snapshot_ref: None,
                    head_snapshot_ref: BlobRef::from_bytes(b"head"),
                    head_totals: vfs::VfsTotals::default(),
                    revision: 0,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            },
            access: WorkspaceAccess::Edit,
        }
    }
}
