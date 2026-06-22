//! Prompt root resolution for CAS-backed VFS mounts.

use std::{collections::BTreeSet, sync::Arc};

use engine::storage::BlobStore;
use thiserror::Error;
use vfs::{VfsMountRecord, VfsMountSource, VfsPath, VfsWorkspaceId, VfsWorkspaceStore};

use crate::{
    fs::{FileSystem, FsError, FsPath, MountedVfsFileSystem},
    prompts::{PromptRoot, PromptRootInput, PromptRootSource},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsPromptRootSpec {
    pub root_id: String,
    pub root_path: VfsPath,
}

impl VfsPromptRootSpec {
    pub fn new(root_id: impl Into<String>, root_path: VfsPath) -> Self {
        Self {
            root_id: root_id.into(),
            root_path,
        }
    }
}

pub struct MountedVfsPromptRoots {
    fs: MountedVfsFileSystem,
    roots: Vec<PromptRoot>,
}

impl MountedVfsPromptRoots {
    pub fn fs(&self) -> &MountedVfsFileSystem {
        &self.fs
    }

    pub fn roots(&self) -> &[PromptRoot] {
        &self.roots
    }

    pub fn into_parts(self) -> (MountedVfsFileSystem, Vec<PromptRoot>) {
        (self.fs, self.roots)
    }

    pub fn inputs(&self) -> Vec<PromptRootInput<'_>> {
        self.roots
            .iter()
            .cloned()
            .map(|root| PromptRootInput {
                root,
                fs: &self.fs as &dyn FileSystem,
            })
            .collect()
    }

    pub async fn existing_directory_inputs(
        &self,
    ) -> Result<Vec<PromptRootInput<'_>>, PromptVfsRootError> {
        let mut inputs = Vec::new();
        for root in &self.roots {
            match self.fs.get_metadata(&root.root_path).await {
                Ok(metadata) if metadata.is_directory => inputs.push(PromptRootInput {
                    root: root.clone(),
                    fs: &self.fs as &dyn FileSystem,
                }),
                Ok(_) | Err(FsError::NotFound { .. }) => {}
                Err(error) => {
                    return Err(PromptVfsRootError::Filesystem {
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(inputs)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PromptVfsRootError {
    #[error("duplicate VFS prompt root id {root_id}")]
    DuplicateRootId { root_id: String },

    #[error("VFS prompt root {root_id} at {root_path} is not under a mounted VFS path")]
    UnmountedRoot { root_id: String, root_path: VfsPath },

    #[error("invalid VFS prompt root {root_id} at {root_path}: {message}")]
    InvalidRootPath {
        root_id: String,
        root_path: VfsPath,
        message: String,
    },

    #[error("failed to build mounted VFS filesystem: {message}")]
    Filesystem { message: String },

    #[error("failed to read VFS workspace {workspace_id}: {message}")]
    Workspace {
        workspace_id: VfsWorkspaceId,
        message: String,
    },
}

pub async fn resolve_mounted_vfs_prompt_roots(
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mounts: Vec<VfsMountRecord>,
    specs: Vec<VfsPromptRootSpec>,
) -> Result<MountedVfsPromptRoots, PromptVfsRootError> {
    validate_specs(&specs)?;
    let fs =
        MountedVfsFileSystem::new(blobs, workspace_store.clone(), mounts).map_err(|error| {
            PromptVfsRootError::Filesystem {
                message: error.to_string(),
            }
        })?;

    let mut roots = Vec::with_capacity(specs.len());
    for spec in specs {
        roots.push(resolve_root(&workspace_store, fs.mounts(), spec).await?);
    }

    Ok(MountedVfsPromptRoots { fs, roots })
}

pub fn conventional_vfs_prompt_root_specs(mounts: &[VfsMountRecord]) -> Vec<VfsPromptRootSpec> {
    let mut specs = Vec::new();
    let mut seen = BTreeSet::new();
    for mount in mounts {
        if matches!(mount.source, VfsMountSource::Workspace { .. }) {
            push_spec(
                &mut specs,
                &mut seen,
                workspace_prompt_root(&mount.mount_path, ".lightspeed/prompts"),
            );
            push_spec(
                &mut specs,
                &mut seen,
                workspace_prompt_root(&mount.mount_path, ".agents/prompts"),
            );
        }
    }
    specs
}

fn push_spec(
    specs: &mut Vec<VfsPromptRootSpec>,
    seen: &mut BTreeSet<String>,
    spec: VfsPromptRootSpec,
) {
    if seen.insert(spec.root_id.clone()) {
        specs.push(spec);
    }
}

fn workspace_prompt_root(mount_path: &VfsPath, suffix: &str) -> VfsPromptRootSpec {
    let path = append_vfs_path(mount_path, suffix);
    VfsPromptRootSpec::new(root_id_for_vfs_path("workspace", &path), path)
}

fn append_vfs_path(base: &VfsPath, suffix: &str) -> VfsPath {
    let path = if base.is_root() {
        format!("/{suffix}")
    } else {
        format!("{}/{suffix}", base.as_str())
    };
    VfsPath::parse(path).expect("conventional VFS prompt root path")
}

fn root_id_for_vfs_path(prefix: &str, path: &VfsPath) -> String {
    let suffix = path.components().join("-");
    if suffix.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}-{suffix}")
    }
}

fn validate_specs(specs: &[VfsPromptRootSpec]) -> Result<(), PromptVfsRootError> {
    let mut seen = BTreeSet::new();
    for spec in specs {
        if !seen.insert(spec.root_id.as_str()) {
            return Err(PromptVfsRootError::DuplicateRootId {
                root_id: spec.root_id.clone(),
            });
        }
    }
    Ok(())
}

async fn resolve_root(
    workspace_store: &Arc<dyn VfsWorkspaceStore>,
    mounts: &[VfsMountRecord],
    spec: VfsPromptRootSpec,
) -> Result<PromptRoot, PromptVfsRootError> {
    let mount = mount_for_root(mounts, &spec.root_path).ok_or_else(|| {
        PromptVfsRootError::UnmountedRoot {
            root_id: spec.root_id.clone(),
            root_path: spec.root_path.clone(),
        }
    })?;
    let root_path = FsPath::new(spec.root_path.as_str()).map_err(|error| {
        PromptVfsRootError::InvalidRootPath {
            root_id: spec.root_id.clone(),
            root_path: spec.root_path.clone(),
            message: error.to_string(),
        }
    })?;
    let source = match &mount.source {
        VfsMountSource::Snapshot { snapshot_ref } => PromptRootSource::MountedSnapshot {
            snapshot_ref: snapshot_ref.clone(),
            mount_path: mount.mount_path.clone(),
        },
        VfsMountSource::Workspace { workspace_id } => {
            let workspace =
                workspace_store
                    .read_workspace(workspace_id)
                    .await
                    .map_err(|error| PromptVfsRootError::Workspace {
                        workspace_id: workspace_id.clone(),
                        message: error.to_string(),
                    })?;
            PromptRootSource::MountedWorkspace {
                workspace_id: workspace_id.clone(),
                workspace_head_ref: workspace.head_snapshot_ref,
                workspace_revision: workspace.revision,
                mount_path: mount.mount_path.clone(),
            }
        }
    };

    Ok(PromptRoot {
        root_id: spec.root_id,
        root_path,
        source,
        access: mount.access,
    })
}

fn mount_for_root<'a>(
    mounts: &'a [VfsMountRecord],
    root_path: &VfsPath,
) -> Option<&'a VfsMountRecord> {
    mounts
        .iter()
        .find(|mount| vfs_path_starts_with(root_path, &mount.mount_path))
}

fn vfs_path_starts_with(path: &VfsPath, base: &VfsPath) -> bool {
    let path_components = path.components();
    let base_components = base.components();
    base_components.len() <= path_components.len()
        && base_components
            .iter()
            .zip(path_components.iter())
            .all(|(base, path)| base == path)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use engine::{BlobRef, SessionId, storage::InMemoryBlobStore};
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, VfsCatalogError, VfsMountAccess, VfsWorkspaceRecord, create_inline_snapshot,
    };

    use super::*;
    use crate::prompts::build_prompt_instructions;

    #[tokio::test]
    async fn resolves_workspace_prompt_roots_and_reads_instructions() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new(
                    ".lightspeed/prompts/instructions.md",
                    b"Project instructions\n".to_vec(),
                )
                .unwrap(),
            ]),
        )
        .await
        .expect("snapshot");
        let workspace_id = VfsWorkspaceId::new("workspace_1");
        workspace_store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                base_snapshot_ref: Some(snapshot.snapshot_ref.clone()),
                head_snapshot_ref: snapshot.snapshot_ref.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("workspace");
        let session_id = SessionId::new("session_1");
        let mounts = vec![mount_record(
            &session_id,
            "/workspace",
            VfsMountSource::Workspace {
                workspace_id: workspace_id.clone(),
            },
            VfsMountAccess::ReadWrite,
        )];
        let specs = conventional_vfs_prompt_root_specs(&mounts);

        let resolved =
            resolve_mounted_vfs_prompt_roots(blobs.clone(), workspace_store, mounts, specs)
                .await
                .expect("resolve roots");
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .expect("existing roots");

        assert_eq!(inputs.len(), 1);
        let build = build_prompt_instructions(
            blobs.as_ref(),
            &inputs,
            crate::prompts::PromptAssemblyLimits::default(),
        )
        .await
        .expect("build prompt");

        assert_eq!(build.entries.len(), 1);
        assert_eq!(
            build.entries[0].content_ref,
            BlobRef::from_bytes(b"Project instructions\n")
        );
        assert!(matches!(
            &build.report.sources[0].source,
            crate::prompts::PromptSourceLocation::MountedWorkspace {
                workspace_id: source_workspace_id,
                workspace_revision,
                source_mount_path,
                prompt_file_path,
                ..
            } if source_workspace_id == &workspace_id
                && *workspace_revision == 0
                && source_mount_path.as_str() == "/workspace"
                && prompt_file_path.as_str() == "/workspace/.lightspeed/prompts/instructions.md"
        ));
        assert!(build.report.sources[0].writable);
    }

    #[test]
    fn conventional_prompt_roots_are_added_for_workspace_mounts_only() {
        let session_id = SessionId::new("session_1");
        let roots = conventional_vfs_prompt_root_specs(&[
            mount_record(
                &session_id,
                "/workspace",
                VfsMountSource::Workspace {
                    workspace_id: VfsWorkspaceId::new("workspace_1"),
                },
                VfsMountAccess::ReadWrite,
            ),
            mount_record(
                &session_id,
                "/skills/system",
                VfsMountSource::Snapshot {
                    snapshot_ref: engine::BlobRef::from_bytes(b"snapshot"),
                },
                VfsMountAccess::ReadOnly,
            ),
        ]);

        assert_eq!(
            roots
                .iter()
                .map(|root| root.root_path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/workspace/.lightspeed/prompts",
                "/workspace/.agents/prompts"
            ]
        );
    }

    fn mount_record(
        session_id: &SessionId,
        mount_path: &str,
        source: VfsMountSource,
        access: VfsMountAccess,
    ) -> VfsMountRecord {
        VfsMountRecord {
            session_id: session_id.clone(),
            mount_path: VfsPath::parse(mount_path).unwrap(),
            source,
            access,
        }
    }

    #[derive(Default)]
    struct TestWorkspaceStore {
        workspaces: std::sync::Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestWorkspaceStore {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let workspace = VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            };
            self.workspaces
                .lock()
                .expect("workspaces")
                .insert(workspace.workspace_id.clone(), workspace.clone());
            Ok(workspace)
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspaces")
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.as_str().to_owned(),
                })
        }

        async fn compare_and_set_head(
            &self,
            _request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            Err(VfsCatalogError::Store {
                message: "not implemented".to_owned(),
            })
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            Err(VfsCatalogError::NotFound {
                kind: "workspace",
                id: workspace_id.as_str().to_owned(),
            })
        }
    }
}
