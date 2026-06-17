//! Skill catalog root resolution for CAS-backed VFS mounts.

use std::{collections::BTreeSet, sync::Arc};

use engine::storage::BlobStore;
use thiserror::Error;
use vfs::{VfsMountRecord, VfsMountSource, VfsPath, VfsWorkspaceId, VfsWorkspaceStore};

use crate::{
    fs::{FileSystem, FsError, FsPath, MountedVfsFileSystem},
    skills::{
        SkillCatalogRoot, SkillCatalogRootInput, SkillCatalogRootSource, SkillScope,
        SkillTrustLevel,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsSkillRootSpec {
    pub root_id: String,
    pub root_path: VfsPath,
    pub trust: SkillTrustLevel,
    pub scope: SkillScope,
}

impl VfsSkillRootSpec {
    pub fn new(
        root_id: impl Into<String>,
        root_path: VfsPath,
        trust: SkillTrustLevel,
        scope: SkillScope,
    ) -> Self {
        Self {
            root_id: root_id.into(),
            root_path,
            trust,
            scope,
        }
    }
}

pub struct MountedVfsSkillCatalogRoots {
    fs: MountedVfsFileSystem,
    roots: Vec<SkillCatalogRoot>,
}

impl MountedVfsSkillCatalogRoots {
    pub fn fs(&self) -> &MountedVfsFileSystem {
        &self.fs
    }

    pub fn roots(&self) -> &[SkillCatalogRoot] {
        &self.roots
    }

    pub fn into_parts(self) -> (MountedVfsFileSystem, Vec<SkillCatalogRoot>) {
        (self.fs, self.roots)
    }

    pub fn inputs(&self) -> Vec<SkillCatalogRootInput<'_>> {
        self.roots
            .iter()
            .cloned()
            .map(|root| SkillCatalogRootInput {
                root,
                fs: &self.fs as &dyn FileSystem,
            })
            .collect()
    }

    pub async fn existing_directory_inputs(
        &self,
    ) -> Result<Vec<SkillCatalogRootInput<'_>>, SkillVfsRootError> {
        let mut inputs = Vec::new();
        for root in &self.roots {
            match self.fs.get_metadata(&root.root_path).await {
                Ok(metadata) if metadata.is_directory => inputs.push(SkillCatalogRootInput {
                    root: root.clone(),
                    fs: &self.fs as &dyn FileSystem,
                }),
                Ok(_) | Err(FsError::NotFound { .. }) => {}
                Err(error) => {
                    return Err(SkillVfsRootError::Filesystem {
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(inputs)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SkillVfsRootError {
    #[error("duplicate VFS skill root id {root_id}")]
    DuplicateRootId { root_id: String },

    #[error("VFS skill root {root_id} at {root_path} is not under a mounted VFS path")]
    UnmountedRoot { root_id: String, root_path: VfsPath },

    #[error("invalid VFS skill root {root_id} at {root_path}: {message}")]
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

pub async fn resolve_mounted_vfs_skill_roots(
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mounts: Vec<VfsMountRecord>,
    specs: Vec<VfsSkillRootSpec>,
) -> Result<MountedVfsSkillCatalogRoots, SkillVfsRootError> {
    validate_specs(&specs)?;
    let fs =
        MountedVfsFileSystem::new(blobs, workspace_store.clone(), mounts).map_err(|error| {
            SkillVfsRootError::Filesystem {
                message: error.to_string(),
            }
        })?;

    let mut roots = Vec::with_capacity(specs.len());
    for spec in specs {
        roots.push(resolve_root(&workspace_store, fs.mounts(), spec).await?);
    }

    Ok(MountedVfsSkillCatalogRoots { fs, roots })
}

pub fn conventional_vfs_skill_root_specs(mounts: &[VfsMountRecord]) -> Vec<VfsSkillRootSpec> {
    let mut specs = Vec::new();
    let mut seen = BTreeSet::new();
    for mount in mounts {
        if is_skills_mount(&mount.mount_path) {
            push_spec(
                &mut specs,
                &mut seen,
                spec_for_skills_mount(&mount.mount_path),
            );
        }
        if matches!(mount.source, VfsMountSource::Workspace { .. }) {
            push_spec(
                &mut specs,
                &mut seen,
                workspace_skill_root(&mount.mount_path, ".lightspeed/skills"),
            );
            push_spec(
                &mut specs,
                &mut seen,
                workspace_skill_root(&mount.mount_path, ".agents/skills"),
            );
        }
    }
    specs
}

fn push_spec(
    specs: &mut Vec<VfsSkillRootSpec>,
    seen: &mut BTreeSet<String>,
    spec: VfsSkillRootSpec,
) {
    if seen.insert(spec.root_id.clone()) {
        specs.push(spec);
    }
}

fn is_skills_mount(path: &VfsPath) -> bool {
    let components = path.components();
    components.first() == Some(&"skills") && components.len() >= 2
}

fn spec_for_skills_mount(path: &VfsPath) -> VfsSkillRootSpec {
    let trust = if path.as_str() == "/skills/system" {
        SkillTrustLevel::System
    } else {
        SkillTrustLevel::User
    };
    VfsSkillRootSpec::new(
        root_id_for_vfs_path("vfs", path),
        path.clone(),
        trust,
        SkillScope::Global,
    )
}

fn workspace_skill_root(mount_path: &VfsPath, suffix: &str) -> VfsSkillRootSpec {
    let path = append_vfs_path(mount_path, suffix);
    VfsSkillRootSpec::new(
        root_id_for_vfs_path("workspace", &path),
        path,
        SkillTrustLevel::Project,
        SkillScope::Global,
    )
}

fn append_vfs_path(base: &VfsPath, suffix: &str) -> VfsPath {
    let path = if base.is_root() {
        format!("/{suffix}")
    } else {
        format!("{}/{suffix}", base.as_str())
    };
    VfsPath::parse(path).expect("conventional VFS skill root path")
}

fn root_id_for_vfs_path(prefix: &str, path: &VfsPath) -> String {
    let suffix = path.components().join("-");
    if suffix.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}-{suffix}")
    }
}

fn validate_specs(specs: &[VfsSkillRootSpec]) -> Result<(), SkillVfsRootError> {
    let mut seen = BTreeSet::new();
    for spec in specs {
        if !seen.insert(spec.root_id.as_str()) {
            return Err(SkillVfsRootError::DuplicateRootId {
                root_id: spec.root_id.clone(),
            });
        }
    }
    Ok(())
}

async fn resolve_root(
    workspace_store: &Arc<dyn VfsWorkspaceStore>,
    mounts: &[VfsMountRecord],
    spec: VfsSkillRootSpec,
) -> Result<SkillCatalogRoot, SkillVfsRootError> {
    let mount = mount_for_root(mounts, &spec.root_path).ok_or_else(|| {
        SkillVfsRootError::UnmountedRoot {
            root_id: spec.root_id.clone(),
            root_path: spec.root_path.clone(),
        }
    })?;
    let root_path = FsPath::new(spec.root_path.as_str()).map_err(|error| {
        SkillVfsRootError::InvalidRootPath {
            root_id: spec.root_id.clone(),
            root_path: spec.root_path.clone(),
            message: error.to_string(),
        }
    })?;
    let source = match &mount.source {
        VfsMountSource::Snapshot { snapshot_ref } => SkillCatalogRootSource::MountedSnapshot {
            snapshot_ref: snapshot_ref.clone(),
            mount_path: mount.mount_path.clone(),
        },
        VfsMountSource::Workspace { workspace_id } => {
            let workspace =
                workspace_store
                    .read_workspace(workspace_id)
                    .await
                    .map_err(|error| SkillVfsRootError::Workspace {
                        workspace_id: workspace_id.clone(),
                        message: error.to_string(),
                    })?;
            SkillCatalogRootSource::MountedWorkspace {
                workspace_id: workspace_id.clone(),
                workspace_head_ref: workspace.head_snapshot_ref,
                mount_path: mount.mount_path.clone(),
            }
        }
    };

    Ok(SkillCatalogRoot {
        root_id: spec.root_id,
        root_path,
        source,
        trust: spec.trust,
        scope: spec.scope,
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
    use engine::{SessionId, storage::InMemoryBlobStore};
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, VfsCatalogError, VfsMountAccess, VfsWorkspaceRecord, create_inline_snapshot,
    };

    use super::*;
    use crate::skills::{SkillLocation, build_skill_catalog};

    #[tokio::test]
    async fn resolves_snapshot_mount_as_skill_catalog_root() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![skill_file(
                "review/SKILL.md",
                "review",
                "Use when reviewing.",
            )]),
        )
        .await
        .expect("snapshot");
        let session_id = SessionId::new("session_1");
        let mounts = vec![mount_record(
            &session_id,
            "/skills/system",
            VfsMountSource::Snapshot {
                snapshot_ref: snapshot.snapshot_ref.clone(),
            },
            VfsMountAccess::ReadOnly,
        )];

        let resolved = resolve_mounted_vfs_skill_roots(
            blobs.clone(),
            workspace_store,
            mounts,
            vec![VfsSkillRootSpec::new(
                "system",
                VfsPath::parse("/skills/system").unwrap(),
                SkillTrustLevel::System,
                SkillScope::Global,
            )],
        )
        .await
        .expect("resolve roots");

        assert_eq!(resolved.roots().len(), 1);
        assert_eq!(resolved.roots()[0].root_path.as_str(), "/skills/system");
        assert!(matches!(
            resolved.roots()[0].source,
            SkillCatalogRootSource::MountedSnapshot { .. }
        ));

        let inputs = resolved.inputs();
        let build = build_skill_catalog(blobs.as_ref(), None, &inputs)
            .await
            .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 1);
        assert_eq!(build.catalog.skills[0].name, "review");
        assert!(matches!(
            &build.catalog.skills[0].location,
            SkillLocation::MountedSnapshot {
                source_snapshot_ref,
                source_mount_path,
                skill_doc_path,
                ..
            } if source_snapshot_ref == &snapshot.snapshot_ref
                && source_mount_path.as_str() == "/skills/system"
                && skill_doc_path.as_str() == "/skills/system/review/SKILL.md"
        ));
    }

    #[tokio::test]
    async fn resolves_workspace_subpath_root_with_observed_head() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![skill_file(
                ".lightspeed/skills/review/SKILL.md",
                "review",
                "Use when reviewing workspace skills.",
            )]),
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

        let resolved = resolve_mounted_vfs_skill_roots(
            blobs.clone(),
            workspace_store,
            mounts,
            vec![VfsSkillRootSpec::new(
                "project",
                VfsPath::parse("/workspace/.lightspeed/skills").unwrap(),
                SkillTrustLevel::Project,
                SkillScope::Global,
            )],
        )
        .await
        .expect("resolve roots");

        assert!(matches!(
            &resolved.roots()[0].source,
            SkillCatalogRootSource::MountedWorkspace {
                workspace_id: resolved_workspace_id,
                workspace_head_ref,
                mount_path,
            } if resolved_workspace_id == &workspace_id
                && workspace_head_ref == &snapshot.snapshot_ref
                && mount_path.as_str() == "/workspace"
        ));

        let inputs = resolved.inputs();
        let build = build_skill_catalog(blobs.as_ref(), None, &inputs)
            .await
            .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 1);
        assert!(matches!(
            &build.catalog.skills[0].location,
            SkillLocation::MountedWorkspace {
                workspace_id: resolved_workspace_id,
                source_mount_path,
                skill_doc_path,
                ..
            } if resolved_workspace_id == &workspace_id
                && source_mount_path.as_str() == "/workspace"
                && skill_doc_path.as_str() == "/workspace/.lightspeed/skills/review/SKILL.md"
        ));
    }

    #[tokio::test]
    async fn rejects_unmounted_skill_root() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());

        let result = resolve_mounted_vfs_skill_roots(
            blobs,
            workspace_store,
            Vec::new(),
            vec![VfsSkillRootSpec::new(
                "system",
                VfsPath::parse("/skills/system").unwrap(),
                SkillTrustLevel::System,
                SkillScope::Global,
            )],
        )
        .await;

        assert_eq!(
            result.err(),
            Some(SkillVfsRootError::UnmountedRoot {
                root_id: "system".to_owned(),
                root_path: VfsPath::parse("/skills/system").unwrap(),
            })
        );
    }

    fn skill_file(path: &str, name: &str, description: &str) -> InlineFile {
        InlineFile::new(
            path,
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody\n").into_bytes(),
        )
        .unwrap()
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
                .expect("workspace lock")
                .insert(workspace.workspace_id.clone(), workspace.clone());
            Ok(workspace)
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }

        async fn compare_and_set_head(
            &self,
            request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            let workspace = workspaces.get_mut(&request.workspace_id).ok_or_else(|| {
                VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                }
            })?;
            if request
                .expected_revision
                .is_some_and(|revision| revision != workspace.revision)
            {
                return Err(VfsCatalogError::RevisionConflict {
                    workspace_id: request.workspace_id,
                    expected_revision: request.expected_revision.unwrap_or_default(),
                    actual_revision: workspace.revision,
                });
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.revision += 1;
            workspace.updated_at_ms = request.updated_at_ms;
            Ok(workspace.clone())
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .remove(workspace_id)
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }
    }
}
