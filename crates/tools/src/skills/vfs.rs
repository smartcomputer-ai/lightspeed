//! Skill catalog root resolution for CAS-backed VFS workspace attachments.

use std::{collections::BTreeSet, sync::Arc};

use engine::storage::BlobStore;
use thiserror::Error;
use vfs::{
    ResolvedWorkspaceAttachment, ResolvedWorkspaceAttachmentTarget, VfsPath, VfsWorkspaceId,
    VfsWorkspaceStore,
};

use crate::{
    fs::{AttachedVfsFileSystem, FileSystem, FsError, FsPath},
    skills::{
        SkillCatalogRoot, SkillCatalogRootInput, SkillCatalogRootSource, SkillLoadWarning,
        SkillLoadWarningKind, SkillScope, SkillTrustLevel,
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

pub struct AttachedVfsSkillCatalogRoots {
    fs: AttachedVfsFileSystem,
    roots: Vec<SkillCatalogRoot>,
    warnings: Vec<SkillLoadWarning>,
}

impl AttachedVfsSkillCatalogRoots {
    pub fn fs(&self) -> &AttachedVfsFileSystem {
        &self.fs
    }

    pub fn roots(&self) -> &[SkillCatalogRoot] {
        &self.roots
    }

    pub fn warnings(&self) -> &[SkillLoadWarning] {
        &self.warnings
    }

    pub fn into_parts(
        self,
    ) -> (
        AttachedVfsFileSystem,
        Vec<SkillCatalogRoot>,
        Vec<SkillLoadWarning>,
    ) {
        (self.fs, self.roots, self.warnings)
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

    #[error("invalid configured VFS skill root {root}: {message}")]
    InvalidConfiguredRoot { root: String, message: String },

    #[error("VFS skill root {root_id} at {root_path} is not under a workspace attachment")]
    UnattachedRoot { root_id: String, root_path: VfsPath },

    #[error("invalid VFS skill root {root_id} at {root_path}: {message}")]
    InvalidRootPath {
        root_id: String,
        root_path: VfsPath,
        message: String,
    },

    #[error("failed to build attached VFS filesystem: {message}")]
    Filesystem { message: String },

    #[error("failed to read VFS workspace {workspace_id}: {message}")]
    Workspace {
        workspace_id: VfsWorkspaceId,
        message: String,
    },
}

pub async fn resolve_attached_vfs_skill_roots(
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    attachments: Vec<ResolvedWorkspaceAttachment>,
    specs: Vec<VfsSkillRootSpec>,
) -> Result<AttachedVfsSkillCatalogRoots, SkillVfsRootError> {
    validate_specs(&specs)?;
    let fs = AttachedVfsFileSystem::new(blobs, workspace_store, attachments).map_err(|error| {
        SkillVfsRootError::Filesystem {
            message: error.to_string(),
        }
    })?;

    let mut roots = Vec::with_capacity(specs.len());
    let mut warnings = Vec::new();
    for spec in specs {
        if let Some(attachment) = attachment_for_root(fs.attachments(), &spec.root_path)
            && let ResolvedWorkspaceAttachmentTarget::Unavailable { reason, .. } =
                &attachment.target
        {
            warnings.push(SkillLoadWarning::new(
                spec.root_id.clone(),
                Some(spec.root_path.to_string()),
                SkillLoadWarningKind::UnavailableWorkspaceAttachment {
                    reason: reason.clone(),
                },
            ));
        }
        if let Some(root) = resolve_root(fs.attachments(), spec).await? {
            roots.push(root);
        }
    }

    Ok(AttachedVfsSkillCatalogRoots {
        fs,
        roots,
        warnings,
    })
}

pub fn configured_vfs_skill_root_specs(
    attachments: &[ResolvedWorkspaceAttachment],
    roots: Option<&[String]>,
) -> Result<Vec<VfsSkillRootSpec>, SkillVfsRootError> {
    let defaults;
    let roots = match roots {
        Some(roots) => roots,
        None => {
            defaults = attachments
                .iter()
                .flat_map(|attachment| {
                    [".agents/skills", ".lightspeed/skills"].map(|suffix| {
                        format!(
                            "{}/{suffix}",
                            attachment.path.as_str().trim_end_matches('/')
                        )
                    })
                })
                .collect::<Vec<_>>();
            &defaults
        }
    };
    roots
        .iter()
        .map(|root| {
            let path =
                VfsPath::parse(root).map_err(|error| SkillVfsRootError::InvalidConfiguredRoot {
                    root: root.clone(),
                    message: error.to_string(),
                })?;
            let trust = if path.as_str() == "/skills/system" {
                SkillTrustLevel::System
            } else if attachment_for_root(attachments, &path).is_some_and(|attachment| {
                matches!(
                    attachment.target,
                    ResolvedWorkspaceAttachmentTarget::AvailableWorkspace { .. }
                )
            }) {
                SkillTrustLevel::Project
            } else {
                SkillTrustLevel::User
            };
            Ok(VfsSkillRootSpec::new(
                root_id_for_vfs_path("config", &path),
                path,
                trust,
                SkillScope::Global,
            ))
        })
        .collect()
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
    attachments: &[ResolvedWorkspaceAttachment],
    spec: VfsSkillRootSpec,
) -> Result<Option<SkillCatalogRoot>, SkillVfsRootError> {
    let attachment = attachment_for_root(attachments, &spec.root_path).ok_or_else(|| {
        SkillVfsRootError::UnattachedRoot {
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
    let source = match &attachment.target {
        ResolvedWorkspaceAttachmentTarget::AvailableSnapshot { snapshot_ref } => {
            SkillCatalogRootSource::AttachedSnapshot {
                snapshot_ref: snapshot_ref.clone(),
                attachment_path: attachment.path.clone(),
            }
        }
        ResolvedWorkspaceAttachmentTarget::AvailableWorkspace { workspace } => {
            SkillCatalogRootSource::AttachedWorkspace {
                workspace_id: workspace.workspace_id.clone(),
                workspace_head_ref: workspace.head_snapshot_ref.clone(),
                attachment_path: attachment.path.clone(),
            }
        }
        ResolvedWorkspaceAttachmentTarget::Unavailable { .. } => return Ok(None),
    };

    Ok(Some(SkillCatalogRoot {
        root_id: spec.root_id,
        root_path,
        source,
        trust: spec.trust,
        scope: spec.scope,
    }))
}

fn attachment_for_root<'a>(
    attachments: &'a [ResolvedWorkspaceAttachment],
    root_path: &VfsPath,
) -> Option<&'a ResolvedWorkspaceAttachment> {
    attachments
        .iter()
        .find(|attachment| vfs_path_starts_with(root_path, &attachment.path))
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
    use engine::{WorkspaceAccess, WorkspaceAttachmentTarget, storage::InMemoryBlobStore};
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, ResolvedWorkspaceAttachment, ResolvedWorkspaceAttachmentTarget,
        VfsCatalogError, VfsWorkspaceRecord, create_inline_snapshot,
    };

    use super::*;
    use crate::skills::{SkillLocation, build_skill_catalog};

    #[tokio::test]
    async fn resolves_snapshot_attachment_as_skill_catalog_root() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            None,
            CreateInlineSnapshotRequest::new(vec![skill_file(
                "review/SKILL.md",
                "review",
                "Use when reviewing.",
            )]),
        )
        .await
        .expect("snapshot");
        let attachments = vec![resolved_attachment(
            "/skills/system",
            ResolvedWorkspaceAttachmentTarget::AvailableSnapshot {
                snapshot_ref: snapshot.snapshot_ref.clone(),
            },
            WorkspaceAccess::Read,
        )];

        let resolved = resolve_attached_vfs_skill_roots(
            blobs.clone(),
            workspace_store,
            attachments,
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
            SkillCatalogRootSource::AttachedSnapshot { .. }
        ));

        let inputs = resolved.inputs();
        let build = build_skill_catalog(blobs.as_ref(), None, &inputs)
            .await
            .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 1);
        assert_eq!(build.catalog.skills[0].name, "review");
        assert!(matches!(
            &build.catalog.skills[0].location,
            SkillLocation::AttachedSnapshot {
                source_snapshot_ref,
                source_attachment_path,
                skill_doc_path,
                ..
            } if source_snapshot_ref == &snapshot.snapshot_ref
                && source_attachment_path.as_str() == "/skills/system"
                && skill_doc_path.as_str() == "/skills/system/review/SKILL.md"
        ));
    }

    #[tokio::test]
    async fn resolves_workspace_subpath_root_with_observed_head() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            None,
            CreateInlineSnapshotRequest::new(vec![skill_file(
                ".lightspeed/skills/review/SKILL.md",
                "review",
                "Use when reviewing workspace skills.",
            )]),
        )
        .await
        .expect("snapshot");
        let workspace_id = VfsWorkspaceId::new("workspace_1");
        let workspace = workspace_store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                display_name: None,
                base_snapshot_ref: Some(snapshot.snapshot_ref.clone()),
                head_snapshot_ref: snapshot.snapshot_ref.clone(),
                head_totals: snapshot.manifest.totals.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("workspace");
        let attachments = vec![resolved_attachment(
            "/workspace",
            ResolvedWorkspaceAttachmentTarget::AvailableWorkspace { workspace },
            WorkspaceAccess::Edit,
        )];

        let resolved = resolve_attached_vfs_skill_roots(
            blobs.clone(),
            workspace_store,
            attachments,
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
            SkillCatalogRootSource::AttachedWorkspace {
                workspace_id: resolved_workspace_id,
                workspace_head_ref,
                attachment_path,
            } if resolved_workspace_id == &workspace_id
                && workspace_head_ref == &snapshot.snapshot_ref
                && attachment_path.as_str() == "/workspace"
        ));

        let inputs = resolved.inputs();
        let build = build_skill_catalog(blobs.as_ref(), None, &inputs)
            .await
            .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 1);
        assert!(matches!(
            &build.catalog.skills[0].location,
            SkillLocation::AttachedWorkspace {
                workspace_id: resolved_workspace_id,
                source_attachment_path,
                skill_doc_path,
                ..
            } if resolved_workspace_id == &workspace_id
                && source_attachment_path.as_str() == "/workspace"
                && skill_doc_path.as_str() == "/workspace/.lightspeed/skills/review/SKILL.md"
        ));
    }

    #[tokio::test]
    async fn rejects_unattached_skill_root() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());

        let result = resolve_attached_vfs_skill_roots(
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
            Some(SkillVfsRootError::UnattachedRoot {
                root_id: "system".to_owned(),
                root_path: VfsPath::parse("/skills/system").unwrap(),
            })
        );
    }

    #[tokio::test]
    async fn unavailable_attachment_becomes_a_source_warning_without_a_root() {
        let resolved = resolve_attached_vfs_skill_roots(
            Arc::new(InMemoryBlobStore::new()),
            Arc::new(TestWorkspaceStore::default()),
            vec![resolved_attachment(
                "/skills/system",
                ResolvedWorkspaceAttachmentTarget::Unavailable {
                    declared_target: WorkspaceAttachmentTarget::Workspace {
                        workspace_id: "deleted".to_owned(),
                    },
                    reason: "workspace was deleted".to_owned(),
                },
                WorkspaceAccess::Edit,
            )],
            vec![VfsSkillRootSpec::new(
                "system",
                VfsPath::parse("/skills/system").unwrap(),
                SkillTrustLevel::System,
                SkillScope::Global,
            )],
        )
        .await
        .expect("unavailable attachments degrade per source");

        assert!(resolved.roots().is_empty());
        assert!(matches!(
            resolved.warnings(),
            [SkillLoadWarning {
                kind: SkillLoadWarningKind::UnavailableWorkspaceAttachment { reason },
                ..
            }] if reason == "workspace was deleted"
        ));
    }

    #[test]
    fn default_skill_roots_cover_attachments_and_explicit_roots_replace_defaults() {
        let attachments = ["/", "/workspace"].map(|path| {
            resolved_attachment(
                path,
                ResolvedWorkspaceAttachmentTarget::AvailableSnapshot {
                    snapshot_ref: engine::BlobRef::from_bytes(b"snapshot"),
                },
                WorkspaceAccess::Read,
            )
        });
        let defaults = configured_vfs_skill_root_specs(&attachments, None).unwrap();
        assert_eq!(
            defaults
                .iter()
                .map(|root| root.root_path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/.agents/skills",
                "/.lightspeed/skills",
                "/workspace/.agents/skills",
                "/workspace/.lightspeed/skills",
            ]
        );
        let overrides =
            configured_vfs_skill_root_specs(&attachments, Some(&["/workspace/custom".into()]))
                .unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].root_path.as_str(), "/workspace/custom");
        assert!(
            configured_vfs_skill_root_specs(&[], None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_configured_roots_do_not_infer_roots_from_attachments() {
        let attachments = vec![resolved_attachment(
            "/skills/system",
            ResolvedWorkspaceAttachmentTarget::Unavailable {
                declared_target: engine::WorkspaceAttachmentTarget::Workspace {
                    workspace_id: "skills".into(),
                },
                reason: "deleted".into(),
            },
            WorkspaceAccess::Read,
        )];
        assert!(
            configured_vfs_skill_root_specs(&attachments, Some(&[]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn configured_skill_roots_preserve_source_locations() {
        let roots = configured_vfs_skill_root_specs(
            &[],
            Some(&["/skills/system".to_owned(), "/custom/skills".to_owned()]),
        )
        .expect("configured roots");

        assert_eq!(roots[0].root_path.as_str(), "/skills/system");
        assert_eq!(roots[0].trust, SkillTrustLevel::System);
        assert_eq!(roots[1].root_path.as_str(), "/custom/skills");
        assert_eq!(roots[1].trust, SkillTrustLevel::User);
    }

    fn skill_file(path: &str, name: &str, description: &str) -> InlineFile {
        InlineFile::new(
            path,
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody\n").into_bytes(),
        )
        .unwrap()
    }

    fn resolved_attachment(
        path: &str,
        target: ResolvedWorkspaceAttachmentTarget,
        access: WorkspaceAccess,
    ) -> ResolvedWorkspaceAttachment {
        ResolvedWorkspaceAttachment {
            path: VfsPath::parse(path).unwrap(),
            target,
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
                display_name: record.display_name,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                head_totals: record.head_totals,
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

        async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
            Ok(self
                .workspaces
                .lock()
                .expect("workspace lock")
                .values()
                .cloned()
                .collect())
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
            if let Some(display_name) = request.display_name {
                workspace.display_name = Some(display_name);
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.head_totals = request.new_head_totals;
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
