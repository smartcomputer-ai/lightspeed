//! Prompt root resolution for CAS-backed VFS workspace links.

use std::{collections::BTreeSet, sync::Arc};

use engine::storage::BlobStore;
use thiserror::Error;
use vfs::{
    ResolvedWorkspaceLink, ResolvedWorkspaceLinkTarget, VfsPath, VfsWorkspaceId, VfsWorkspaceStore,
};

use crate::{
    fs::{FileSystem, FsError, FsPath, LinkedVfsFileSystem},
    prompts::{PromptRoot, PromptRootInput, PromptRootSource, PromptWarning, PromptWarningKind},
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

pub struct LinkedVfsPromptRoots {
    fs: LinkedVfsFileSystem,
    roots: Vec<PromptRoot>,
    warnings: Vec<PromptWarning>,
}

impl LinkedVfsPromptRoots {
    pub fn fs(&self) -> &LinkedVfsFileSystem {
        &self.fs
    }

    pub fn roots(&self) -> &[PromptRoot] {
        &self.roots
    }

    pub fn warnings(&self) -> &[PromptWarning] {
        &self.warnings
    }

    pub fn into_parts(self) -> (LinkedVfsFileSystem, Vec<PromptRoot>, Vec<PromptWarning>) {
        (self.fs, self.roots, self.warnings)
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

    #[error("invalid configured VFS prompt root {root}: {message}")]
    InvalidConfiguredRoot { root: String, message: String },

    #[error("VFS prompt root {root_id} at {root_path} is not under a workspace link")]
    UnlinkedRoot { root_id: String, root_path: VfsPath },

    #[error("invalid VFS prompt root {root_id} at {root_path}: {message}")]
    InvalidRootPath {
        root_id: String,
        root_path: VfsPath,
        message: String,
    },

    #[error("failed to build linked VFS filesystem: {message}")]
    Filesystem { message: String },

    #[error("failed to read VFS workspace {workspace_id}: {message}")]
    Workspace {
        workspace_id: VfsWorkspaceId,
        message: String,
    },
}

pub async fn resolve_linked_vfs_prompt_roots(
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    links: Vec<ResolvedWorkspaceLink>,
    specs: Vec<VfsPromptRootSpec>,
) -> Result<LinkedVfsPromptRoots, PromptVfsRootError> {
    validate_specs(&specs)?;
    let fs = LinkedVfsFileSystem::new(blobs, workspace_store, links).map_err(|error| {
        PromptVfsRootError::Filesystem {
            message: error.to_string(),
        }
    })?;

    let mut roots = Vec::with_capacity(specs.len());
    let mut warnings = Vec::new();
    for spec in specs {
        if let Some(link) = link_for_root(fs.links(), &spec.root_path)
            && let ResolvedWorkspaceLinkTarget::Unavailable { reason, .. } = &link.target
        {
            warnings.push(PromptWarning::new(
                spec.root_id.clone(),
                Some(spec.root_path.to_string()),
                PromptWarningKind::UnavailableWorkspaceLink {
                    reason: reason.clone(),
                },
            ));
        }
        if let Some(root) = resolve_root(fs.links(), spec).await? {
            roots.push(root);
        }
    }

    Ok(LinkedVfsPromptRoots {
        fs,
        roots,
        warnings,
    })
}

pub fn conventional_vfs_prompt_root_specs(
    links: &[ResolvedWorkspaceLink],
) -> Vec<VfsPromptRootSpec> {
    let mut specs = Vec::new();
    let mut seen = BTreeSet::new();
    for link in links {
        push_spec(
            &mut specs,
            &mut seen,
            workspace_prompt_root(&link.path, ".lightspeed/prompts"),
        );
        push_spec(
            &mut specs,
            &mut seen,
            workspace_prompt_root(&link.path, ".agents/prompts"),
        );
    }
    specs
}

pub fn configured_vfs_prompt_root_specs(
    links: &[ResolvedWorkspaceLink],
    roots: Option<&[String]>,
) -> Result<Vec<VfsPromptRootSpec>, PromptVfsRootError> {
    let Some(roots) = roots else {
        return Ok(conventional_vfs_prompt_root_specs(links));
    };
    roots
        .iter()
        .map(|root| {
            let path = VfsPath::parse(root).map_err(|error| {
                PromptVfsRootError::InvalidConfiguredRoot {
                    root: root.clone(),
                    message: error.to_string(),
                }
            })?;
            Ok(VfsPromptRootSpec::new(
                root_id_for_vfs_path("config", &path),
                path,
            ))
        })
        .collect()
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

fn workspace_prompt_root(link_path: &VfsPath, suffix: &str) -> VfsPromptRootSpec {
    let path = append_vfs_path(link_path, suffix);
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
    links: &[ResolvedWorkspaceLink],
    spec: VfsPromptRootSpec,
) -> Result<Option<PromptRoot>, PromptVfsRootError> {
    let link =
        link_for_root(links, &spec.root_path).ok_or_else(|| PromptVfsRootError::UnlinkedRoot {
            root_id: spec.root_id.clone(),
            root_path: spec.root_path.clone(),
        })?;
    let root_path = FsPath::new(spec.root_path.as_str()).map_err(|error| {
        PromptVfsRootError::InvalidRootPath {
            root_id: spec.root_id.clone(),
            root_path: spec.root_path.clone(),
            message: error.to_string(),
        }
    })?;
    let source = match &link.target {
        ResolvedWorkspaceLinkTarget::AvailableSnapshot { snapshot_ref } => {
            PromptRootSource::LinkedSnapshot {
                snapshot_ref: snapshot_ref.clone(),
                link_path: link.path.clone(),
            }
        }
        ResolvedWorkspaceLinkTarget::AvailableWorkspace { workspace } => {
            PromptRootSource::LinkedWorkspace {
                workspace_id: workspace.workspace_id.clone(),
                workspace_head_ref: workspace.head_snapshot_ref.clone(),
                workspace_revision: workspace.revision,
                link_path: link.path.clone(),
            }
        }
        ResolvedWorkspaceLinkTarget::Unavailable { .. } => return Ok(None),
    };

    Ok(Some(PromptRoot {
        root_id: spec.root_id,
        root_path,
        source,
        access: link.access,
    }))
}

fn link_for_root<'a>(
    links: &'a [ResolvedWorkspaceLink],
    root_path: &VfsPath,
) -> Option<&'a ResolvedWorkspaceLink> {
    links
        .iter()
        .find(|link| vfs_path_starts_with(root_path, &link.path))
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
    use engine::{BlobRef, WorkspaceLinkAccess, storage::InMemoryBlobStore};
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, ResolvedWorkspaceLink, ResolvedWorkspaceLinkTarget, VfsCatalogError,
        VfsWorkspaceRecord, create_inline_snapshot,
    };

    use super::*;
    use crate::prompts::build_prompt_instructions;

    #[tokio::test]
    async fn resolves_workspace_prompt_roots_and_reads_instructions() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let workspace_store = Arc::new(TestWorkspaceStore::default());
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            None,
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
        let links = vec![resolved_link(
            "/workspace",
            ResolvedWorkspaceLinkTarget::AvailableWorkspace { workspace },
            WorkspaceLinkAccess::ReadWrite,
        )];
        let specs = conventional_vfs_prompt_root_specs(&links);

        let resolved =
            resolve_linked_vfs_prompt_roots(blobs.clone(), workspace_store, links, specs)
                .await
                .expect("resolve roots");
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .expect("existing roots");

        assert_eq!(inputs.len(), 1);
        let build = build_prompt_instructions(
            blobs.as_ref(),
            None,
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
            crate::prompts::PromptSourceLocation::LinkedWorkspace {
                workspace_id: source_workspace_id,
                workspace_revision,
                source_link_path,
                prompt_file_path,
                ..
            } if source_workspace_id == &workspace_id
                && *workspace_revision == 0
                && source_link_path.as_str() == "/workspace"
                && prompt_file_path.as_str() == "/workspace/.lightspeed/prompts/instructions.md"
        ));
        assert!(build.report.sources[0].writable);
    }

    #[test]
    fn conventional_prompt_roots_cover_workspace_and_snapshot_links() {
        let roots = conventional_vfs_prompt_root_specs(&[
            resolved_link(
                "/workspace",
                ResolvedWorkspaceLinkTarget::AvailableWorkspace {
                    workspace: workspace_record("workspace_1", BlobRef::from_bytes(b"head")),
                },
                WorkspaceLinkAccess::ReadWrite,
            ),
            resolved_link(
                "/skills/system",
                ResolvedWorkspaceLinkTarget::AvailableSnapshot {
                    snapshot_ref: engine::BlobRef::from_bytes(b"snapshot"),
                },
                WorkspaceLinkAccess::ReadOnly,
            ),
        ]);

        assert_eq!(
            roots
                .iter()
                .map(|root| root.root_path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/workspace/.lightspeed/prompts",
                "/workspace/.agents/prompts",
                "/skills/system/.lightspeed/prompts",
                "/skills/system/.agents/prompts"
            ]
        );
    }

    #[test]
    fn configured_prompt_roots_replace_conventional_roots() {
        let roots = configured_vfs_prompt_root_specs(
            &[],
            Some(&["/custom/prompts".to_owned(), "/shared/prompts".to_owned()]),
        )
        .expect("configured roots");

        assert_eq!(
            roots
                .iter()
                .map(|root| root.root_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/custom/prompts", "/shared/prompts"]
        );
    }

    fn resolved_link(
        path: &str,
        target: ResolvedWorkspaceLinkTarget,
        access: WorkspaceLinkAccess,
    ) -> ResolvedWorkspaceLink {
        ResolvedWorkspaceLink {
            path: VfsPath::parse(path).unwrap(),
            target,
            access,
        }
    }

    fn workspace_record(workspace_id: &str, head_snapshot_ref: BlobRef) -> VfsWorkspaceRecord {
        VfsWorkspaceRecord {
            workspace_id: VfsWorkspaceId::new(workspace_id),
            display_name: None,
            base_snapshot_ref: None,
            head_snapshot_ref,
            head_totals: vfs::VfsTotals::default(),
            revision: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
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

        async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
            Ok(self
                .workspaces
                .lock()
                .expect("workspaces")
                .values()
                .cloned()
                .collect())
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
