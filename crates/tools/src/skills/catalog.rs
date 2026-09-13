use std::{cmp::Ordering, collections::BTreeSet};

use super::SkillId;
use crate::catalog::{
    SKILL_CATALOG_CONTEXT_KEY, catalog_context_input, catalog_publication_command,
};
use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand,
    storage::{BlobGraphStore, BlobStore, BlobStoreError, record_contains_edges},
};
use serde::Serialize;
use thiserror::Error;
use vfs::VfsPath;

use crate::{
    fs::{FileSystem, FsError, FsPath},
    skills::{
        SkillCatalogBuildRecord, SkillCatalogRoot, SkillCatalogRootSource, SkillCatalogSnapshot,
        SkillCatalogSourceFingerprint, SkillCatalogSourceInput, SkillDependencies, SkillInterface,
        SkillLoadWarning, SkillLoadWarningKind, SkillLocation, SkillMetadata, SkillSource,
        parse_skill_frontmatter,
    },
};

const SOURCE_FINGERPRINT_SCHEMA_VERSION: &str = "lightspeed.skills.catalog.source_fingerprint.v1";
const PARSER_VERSION: &str = "lightspeed.skills.frontmatter_parser.v1";

pub struct SkillCatalogRootInput<'a> {
    pub root: SkillCatalogRoot,
    pub fs: &'a dyn FileSystem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCatalogBuild {
    pub catalog_ref: BlobRef,
    pub catalog: SkillCatalogSnapshot,
    pub catalog_bytes: Vec<u8>,
    pub build_record: SkillCatalogBuildRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCatalogPublication {
    pub build: SkillCatalogBuild,
    pub command: Option<CoreAgentCommand>,
}

#[derive(Debug, Error)]
pub enum SkillCatalogError {
    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("failed to encode skill catalog: {message}")]
    Encode { message: String },

    #[error("invalid skill catalog path {path}: {message}")]
    InvalidPath { path: String, message: String },
}

pub struct SkillCatalogBuilder<'a> {
    blobs: &'a dyn BlobStore,
    blob_graph: Option<&'a dyn BlobGraphStore>,
    roots: Vec<SkillCatalogRootInput<'a>>,
}

impl<'a> SkillCatalogBuilder<'a> {
    pub fn new(blobs: &'a dyn BlobStore) -> Self {
        Self {
            blobs,
            blob_graph: None,
            roots: Vec::new(),
        }
    }

    pub fn with_blob_graph(mut self, blob_graph: Option<&'a dyn BlobGraphStore>) -> Self {
        self.blob_graph = blob_graph;
        self
    }

    pub fn with_root(mut self, root: SkillCatalogRootInput<'a>) -> Self {
        self.roots.push(root);
        self
    }

    pub async fn build(self) -> Result<SkillCatalogBuild, SkillCatalogError> {
        build_skill_catalog(self.blobs, self.blob_graph, &self.roots).await
    }
}

pub async fn build_skill_catalog(
    blobs: &dyn BlobStore,
    blob_graph: Option<&dyn BlobGraphStore>,
    roots: &[SkillCatalogRootInput<'_>],
) -> Result<SkillCatalogBuild, SkillCatalogError> {
    build_skill_catalog_with_warnings(blobs, blob_graph, roots, Vec::new()).await
}

/// Retain source snapshots for pinned catalogs. Discovery metadata carries no
/// skill body references; ordinary file reads retain their own recorded bytes.
pub fn skill_catalog_blob_refs(catalog: &SkillCatalogSnapshot) -> BTreeSet<BlobRef> {
    let mut refs = BTreeSet::new();
    for skill in &catalog.skills {
        if let SkillSource::Snapshot { snapshot_ref, .. } = &skill.source {
            refs.insert(snapshot_ref.clone());
        }
        if let SkillLocation::AttachedSnapshot {
            source_snapshot_ref,
            ..
        } = &skill.location
        {
            refs.insert(source_snapshot_ref.clone());
        }
    }
    refs
}

pub async fn build_skill_catalog_with_warnings(
    blobs: &dyn BlobStore,
    blob_graph: Option<&dyn BlobGraphStore>,
    roots: &[SkillCatalogRootInput<'_>],
    mut warnings: Vec<SkillLoadWarning>,
) -> Result<SkillCatalogBuild, SkillCatalogError> {
    let mut sorted_roots = roots.iter().collect::<Vec<_>>();
    sorted_roots.sort_by(compare_roots);

    let mut skills = Vec::new();
    let mut source_inputs = Vec::new();

    for input in sorted_roots {
        let scan = scan_root(input).await?;
        skills.extend(scan.skills);
        warnings.extend(scan.warnings);
        source_inputs.push(scan.source_input);
    }

    skills.sort_by(|left, right| left.skill_id.as_str().cmp(right.skill_id.as_str()));
    warnings.sort_by(compare_warnings);
    source_inputs.sort_by(compare_source_inputs);

    let catalog = SkillCatalogSnapshot::new(skills, warnings);
    let catalog_bytes = encode_json(&catalog)?;
    let catalog_ref = blobs.put_bytes(catalog_bytes.clone()).await?;
    record_contains_edges(blob_graph, &catalog_ref, skill_catalog_blob_refs(&catalog)).await?;
    let source_fingerprint = source_fingerprint(source_inputs)?;
    let build_record = SkillCatalogBuildRecord::new(catalog_ref.clone(), source_fingerprint);

    Ok(SkillCatalogBuild {
        catalog_ref,
        catalog,
        catalog_bytes,
        build_record,
    })
}

pub async fn prepare_skill_catalog_publication(
    blobs: &dyn BlobStore,
    blob_graph: Option<&dyn BlobGraphStore>,
    current: Option<&ContextEntryInput>,
    roots: &[SkillCatalogRootInput<'_>],
) -> Result<SkillCatalogPublication, SkillCatalogError> {
    prepare_skill_catalog_publication_with_warnings(blobs, blob_graph, current, roots, Vec::new())
        .await
}

pub async fn prepare_skill_catalog_publication_with_warnings(
    blobs: &dyn BlobStore,
    blob_graph: Option<&dyn BlobGraphStore>,
    current: Option<&ContextEntryInput>,
    roots: &[SkillCatalogRootInput<'_>],
    warnings: Vec<SkillLoadWarning>,
) -> Result<SkillCatalogPublication, SkillCatalogError> {
    let build = build_skill_catalog_with_warnings(blobs, blob_graph, roots, warnings).await?;
    let entry =
        skill_catalog_context_input(blobs, &build.catalog, build.catalog_ref.clone()).await?;
    let command = catalog_publication_command(current, SKILL_CATALOG_CONTEXT_KEY, entry);

    Ok(SkillCatalogPublication { build, command })
}

pub async fn skill_catalog_context_input(
    blobs: &dyn BlobStore,
    catalog: &SkillCatalogSnapshot,
    catalog_ref: BlobRef,
) -> Result<ContextEntryInput, BlobStoreError> {
    let mut entry = catalog_context_input(
        blobs,
        "VFS skill catalog",
        super::catalog_text::skill_catalog_text(catalog),
        catalog_ref,
    )
    .await?;
    entry.origin = Some("runtime.vfs.skills".to_owned());
    Ok(entry)
}

async fn scan_root(input: &SkillCatalogRootInput<'_>) -> Result<RootScanResult, SkillCatalogError> {
    let mut scan = RootScan::new(input);
    let entries = match input.root.fs_read_directory(input.fs).await {
        Ok(entries) => entries,
        Err(error) => {
            scan.warn(
                None,
                SkillLoadWarningKind::Filesystem {
                    message: error.to_string(),
                },
            );
            return Ok(scan.finish(input));
        }
    };

    let mut entries = entries
        .into_iter()
        .filter(|entry| entry.is_directory)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));

    for entry in entries {
        let skill_dir_path = match input.root.root_path.join(&entry.file_name) {
            Ok(path) => path,
            Err(error) => {
                let path = format!("{}/{}", input.root.root_path.as_str(), entry.file_name);
                scan.warn(
                    Some(path.clone()),
                    SkillLoadWarningKind::Filesystem {
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };
        let skill_doc_path = match skill_dir_path.join("SKILL.md") {
            Ok(path) => path,
            Err(error) => {
                scan.warn(
                    Some(skill_dir_path.as_str().to_owned()),
                    SkillLoadWarningKind::Filesystem {
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };

        let markdown = match input.fs.read_file_text(&skill_doc_path).await {
            Ok(markdown) => markdown,
            Err(FsError::NotFound { .. }) => {
                scan.warn(
                    Some(skill_doc_path.as_str().to_owned()),
                    SkillLoadWarningKind::MissingSkillDoc,
                );
                continue;
            }
            Err(error) => {
                scan.warn(
                    Some(skill_doc_path.as_str().to_owned()),
                    SkillLoadWarningKind::Filesystem {
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };

        let frontmatter = match parse_skill_frontmatter(&markdown) {
            Ok(frontmatter) => frontmatter,
            Err(error) => {
                scan.warn(
                    Some(skill_doc_path.as_str().to_owned()),
                    SkillLoadWarningKind::InvalidSkillDoc {
                        message: error.to_string(),
                    },
                );
                continue;
            }
        };

        match metadata_for_skill(input, &skill_dir_path, &skill_doc_path, frontmatter) {
            Ok(metadata) => scan.skills.push(metadata),
            Err(error) => {
                scan.warn(
                    Some(skill_doc_path.as_str().to_owned()),
                    SkillLoadWarningKind::InvalidSkillDoc {
                        message: error.to_string(),
                    },
                );
            }
        }
    }

    Ok(scan.finish(input))
}

fn metadata_for_skill(
    input: &SkillCatalogRootInput<'_>,
    skill_dir_path: &FsPath,
    skill_doc_path: &FsPath,
    frontmatter: crate::skills::SkillFrontmatter,
) -> Result<SkillMetadata, SkillCatalogError> {
    let skill_id = skill_id_for_path(&input.root, skill_doc_path);
    let location = location_for_skill(&input.root, skill_dir_path, skill_doc_path)?;
    let source = source_for_root(&input.root);
    let short_description = frontmatter.short_description;

    Ok(SkillMetadata {
        skill_id,
        name: frontmatter.name,
        description: frontmatter.description,
        short_description: short_description.clone(),
        source,
        scope: input.root.scope,
        enabled: true,
        trust: input.root.trust,
        interface: Some(SkillInterface {
            display_name: None,
            short_description,
        }),
        dependencies: SkillDependencies::default(),
        location,
    })
}

fn source_for_root(root: &SkillCatalogRoot) -> SkillSource {
    match &root.source {
        SkillCatalogRootSource::AttachedSnapshot { snapshot_ref, .. } => SkillSource::Snapshot {
            root_id: root.root_id.clone(),
            snapshot_ref: snapshot_ref.clone(),
        },
        SkillCatalogRootSource::AttachedWorkspace { workspace_id, .. } => SkillSource::Workspace {
            root_id: root.root_id.clone(),
            workspace_id: workspace_id.clone(),
        },
    }
}

fn location_for_skill(
    root: &SkillCatalogRoot,
    skill_dir_path: &FsPath,
    skill_doc_path: &FsPath,
) -> Result<SkillLocation, SkillCatalogError> {
    match &root.source {
        SkillCatalogRootSource::AttachedSnapshot {
            snapshot_ref,
            attachment_path,
        } => Ok(SkillLocation::AttachedSnapshot {
            source_snapshot_ref: snapshot_ref.clone(),
            source_attachment_path: attachment_path.clone(),
            skill_dir_path: vfs_path(skill_dir_path)?,
            skill_doc_path: vfs_path(skill_doc_path)?,
        }),
        SkillCatalogRootSource::AttachedWorkspace {
            workspace_id,
            workspace_head_ref: _,
            attachment_path,
        } => Ok(SkillLocation::AttachedWorkspace {
            workspace_id: workspace_id.clone(),
            source_attachment_path: attachment_path.clone(),
            skill_dir_path: vfs_path(skill_dir_path)?,
            skill_doc_path: vfs_path(skill_doc_path)?,
        }),
    }
}

fn vfs_path(path: &FsPath) -> Result<VfsPath, SkillCatalogError> {
    VfsPath::parse(path.as_str()).map_err(|error| SkillCatalogError::InvalidPath {
        path: path.as_str().to_owned(),
        message: error.to_string(),
    })
}

fn skill_id_for_path(root: &SkillCatalogRoot, skill_doc_path: &FsPath) -> SkillId {
    let source_digest = short_digest(format!("{}|{}", root.root_id, source_key(&root.source)));
    let path_digest = short_digest(skill_doc_path.as_str());
    SkillId::new(format!("skill:{source_digest}:{path_digest}"))
}

fn source_key(source: &SkillCatalogRootSource) -> String {
    match source {
        SkillCatalogRootSource::AttachedSnapshot { snapshot_ref, .. } => {
            format!("snapshot:{snapshot_ref}")
        }
        SkillCatalogRootSource::AttachedWorkspace {
            workspace_id,
            workspace_head_ref: _,
            attachment_path: _,
        } => format!("workspace:{workspace_id}"),
    }
}

fn short_digest(value: impl AsRef<[u8]>) -> String {
    let digest = BlobRef::from_bytes(value.as_ref());
    digest.as_str()["sha256:".len().."sha256:".len() + 16].to_owned()
}

#[derive(Debug)]
struct RootScan {
    root_id: String,
    skills: Vec<SkillMetadata>,
    warnings: Vec<SkillLoadWarning>,
}

impl RootScan {
    fn new(input: &SkillCatalogRootInput<'_>) -> Self {
        Self {
            root_id: input.root.root_id.clone(),
            skills: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, path: Option<String>, kind: SkillLoadWarningKind) {
        self.warnings
            .push(SkillLoadWarning::new(self.root_id.clone(), path, kind));
    }

    fn finish(self, input: &SkillCatalogRootInput<'_>) -> RootScanResult {
        let source_input = source_input_for_root(input);
        RootScanResult {
            skills: self.skills,
            warnings: self.warnings,
            source_input,
        }
    }
}

struct RootScanResult {
    skills: Vec<SkillMetadata>,
    warnings: Vec<SkillLoadWarning>,
    source_input: SkillCatalogSourceInput,
}

fn source_input_for_root(input: &SkillCatalogRootInput<'_>) -> SkillCatalogSourceInput {
    match &input.root.source {
        SkillCatalogRootSource::AttachedSnapshot { snapshot_ref, .. } => {
            SkillCatalogSourceInput::SnapshotRoot {
                root_id: input.root.root_id.clone(),
                snapshot_ref: snapshot_ref.clone(),
                root_path: vfs_path(&input.root.root_path).unwrap_or_else(|_| VfsPath::root()),
            }
        }
        SkillCatalogRootSource::AttachedWorkspace {
            workspace_id,
            workspace_head_ref,
            attachment_path: _,
        } => SkillCatalogSourceInput::WorkspaceRoot {
            root_id: input.root.root_id.clone(),
            workspace_id: workspace_id.clone(),
            workspace_head_ref: workspace_head_ref.clone(),
            root_path: vfs_path(&input.root.root_path).unwrap_or_else(|_| VfsPath::root()),
        },
    }
}

fn source_fingerprint(
    inputs: Vec<SkillCatalogSourceInput>,
) -> Result<SkillCatalogSourceFingerprint, SkillCatalogError> {
    let payload = SourceFingerprintPayload {
        schema_version: SOURCE_FINGERPRINT_SCHEMA_VERSION,
        parser_version: PARSER_VERSION,
        inputs: &inputs,
    };
    let bytes = encode_json(&payload)?;
    Ok(SkillCatalogSourceFingerprint::sha256(
        BlobRef::from_bytes(&bytes),
        inputs,
    ))
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, SkillCatalogError> {
    serde_json::to_vec(value).map_err(|error| SkillCatalogError::Encode {
        message: error.to_string(),
    })
}

fn compare_roots(
    left: &&SkillCatalogRootInput<'_>,
    right: &&SkillCatalogRootInput<'_>,
) -> Ordering {
    left.root.root_id.cmp(&right.root.root_id).then_with(|| {
        left.root
            .root_path
            .as_str()
            .cmp(right.root.root_path.as_str())
    })
}

fn compare_warnings(left: &SkillLoadWarning, right: &SkillLoadWarning) -> Ordering {
    left.root_id
        .cmp(&right.root_id)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| warning_kind_key(&left.kind).cmp(&warning_kind_key(&right.kind)))
}

fn warning_kind_key(kind: &SkillLoadWarningKind) -> String {
    serde_json::to_string(kind).unwrap_or_else(|_| format!("{kind:?}"))
}

fn compare_source_inputs(
    left: &SkillCatalogSourceInput,
    right: &SkillCatalogSourceInput,
) -> Ordering {
    source_input_key(left).cmp(&source_input_key(right))
}

fn source_input_key(input: &SkillCatalogSourceInput) -> String {
    match input {
        SkillCatalogSourceInput::SnapshotRoot { root_id, .. }
        | SkillCatalogSourceInput::WorkspaceRoot { root_id, .. } => root_id.clone(),
    }
}

#[derive(Serialize)]
struct SourceFingerprintPayload<'a> {
    schema_version: &'static str,
    parser_version: &'static str,
    inputs: &'a [SkillCatalogSourceInput],
}

trait RootReadDirectory {
    async fn fs_read_directory(
        &self,
        fs: &dyn FileSystem,
    ) -> Result<Vec<crate::fs::ReadDirectoryEntry>, FsError>;
}

impl RootReadDirectory for SkillCatalogRoot {
    async fn fs_read_directory(
        &self,
        fs: &dyn FileSystem,
    ) -> Result<Vec<crate::fs::ReadDirectoryEntry>, FsError> {
        fs.read_directory(&self.root_path).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        BlobRef,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use vfs::VfsWorkspaceId;

    use super::*;
    use crate::{
        fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
        skills::{SkillCatalogSource, SkillScope, SkillTrustLevel, VFS_SKILL_CATALOG_ID},
    };

    #[tokio::test]
    async fn builds_catalog_from_valid_skill_directories() {
        let fs = skill_fs(&[
            (
                "/skills/deploy-review/SKILL.md",
                skill_doc("deploy-review", "Use when reviewing deployment risk."),
            ),
            (
                "/skills/release-notes/SKILL.md",
                skill_doc("release-notes", "Use when writing release notes."),
            ),
        ])
        .await;
        let blobs = Arc::new(InMemoryBlobStore::new());
        let build = build_skill_catalog(
            blobs.as_ref(),
            None,
            &[root_input(
                &fs,
                SkillCatalogRoot {
                    root_id: "system".to_owned(),
                    root_path: FsPath::new("/skills").unwrap(),
                    source: SkillCatalogRootSource::AttachedSnapshot {
                        snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                        attachment_path: VfsPath::parse("/skills").unwrap(),
                    },
                    trust: SkillTrustLevel::System,
                    scope: SkillScope::Global,
                },
            )],
        )
        .await
        .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 2);
        assert_eq!(build.catalog.catalog_id, VFS_SKILL_CATALOG_ID);
        assert_eq!(build.catalog.source, SkillCatalogSource::Vfs);
        assert!(build.catalog.warnings.is_empty());
        assert_eq!(build.catalog_ref, BlobRef::from_bytes(&build.catalog_bytes));
        assert_eq!(
            blobs
                .read_bytes(&build.catalog_ref)
                .await
                .expect("read catalog blob"),
            build.catalog_bytes
        );
        assert!(matches!(
            build.catalog.skills[0].location,
            SkillLocation::AttachedSnapshot { .. }
        ));
    }

    /// The recorded edge set must equal every ref the catalog document
    /// embeds: the immutable source snapshot manifests.
    #[tokio::test]
    async fn catalog_writes_record_an_edge_for_every_embedded_ref() {
        let fs = skill_fs(&[
            (
                "/skills/deploy-review/SKILL.md",
                skill_doc("deploy-review", "Use when reviewing deployment risk."),
            ),
            (
                "/skills/release-notes/SKILL.md",
                skill_doc("release-notes", "Use when writing release notes."),
            ),
        ])
        .await;
        let blobs = Arc::new(InMemoryBlobStore::new());
        // In production the root's snapshot manifest is a stored blob.
        let snapshot_ref = blobs
            .put_bytes(b"snapshot manifest".to_vec())
            .await
            .expect("put snapshot");
        let build = build_skill_catalog(
            blobs.as_ref(),
            Some(blobs.as_ref()),
            &[root_input(
                &fs,
                SkillCatalogRoot {
                    root_id: "system".to_owned(),
                    root_path: FsPath::new("/skills").unwrap(),
                    source: SkillCatalogRootSource::AttachedSnapshot {
                        snapshot_ref: snapshot_ref.clone(),
                        attachment_path: VfsPath::parse("/skills").unwrap(),
                    },
                    trust: SkillTrustLevel::System,
                    scope: SkillScope::Global,
                },
            )],
        )
        .await
        .expect("build catalog");

        let embedded = engine::storage::collect_blob_refs(
            &serde_json::from_slice(&build.catalog_bytes).expect("catalog json"),
        );
        let recorded: std::collections::BTreeSet<BlobRef> = blobs
            .edges()
            .into_iter()
            .inspect(|edge| assert_eq!(edge.parent, build.catalog_ref))
            .map(|edge| edge.child)
            .collect();
        assert_eq!(recorded, embedded);
        assert_eq!(recorded, skill_catalog_blob_refs(&build.catalog));
        assert!(embedded.contains(&snapshot_ref));
        assert_eq!(
            embedded.len(),
            1,
            "only the source snapshot manifest is retained by the catalog"
        );
        assert!(
            BlobRef::parse(&build.build_record.source_fingerprint.digest).is_err(),
            "fingerprints must not look like blob refs"
        );
    }

    #[tokio::test]
    async fn duplicate_names_are_allowed_across_roots() {
        let first = skill_fs(&[(
            "/skills-one/review/SKILL.md",
            skill_doc("review", "Use when reviewing one."),
        )])
        .await;
        let second = skill_fs(&[(
            "/skills-two/review/SKILL.md",
            skill_doc("review", "Use when reviewing two."),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();
        let build = build_skill_catalog(
            &blobs,
            None,
            &[
                root_input(
                    &first,
                    SkillCatalogRoot {
                        root_id: "one".to_owned(),
                        root_path: FsPath::new("/skills-one").unwrap(),
                        source: SkillCatalogRootSource::AttachedSnapshot {
                            snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                            attachment_path: VfsPath::parse("/skills-one").unwrap(),
                        },
                        trust: SkillTrustLevel::System,
                        scope: SkillScope::Global,
                    },
                ),
                root_input(
                    &second,
                    SkillCatalogRoot {
                        root_id: "two".to_owned(),
                        root_path: FsPath::new("/skills-two").unwrap(),
                        source: SkillCatalogRootSource::AttachedSnapshot {
                            snapshot_ref: BlobRef::from_bytes(b"snapshot-2"),
                            attachment_path: VfsPath::parse("/skills-two").unwrap(),
                        },
                        trust: SkillTrustLevel::User,
                        scope: SkillScope::Global,
                    },
                ),
            ],
        )
        .await
        .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 2);
        assert_eq!(build.catalog.skills[0].name, "review");
        assert_eq!(build.catalog.skills[1].name, "review");
        assert_ne!(
            build.catalog.skills[0].skill_id,
            build.catalog.skills[1].skill_id
        );
    }

    #[tokio::test]
    async fn invalid_skill_docs_are_reported_as_warnings() {
        let fs = skill_fs(&[
            ("/skills/good/SKILL.md", skill_doc("good", "Use when good.")),
            (
                "/skills/bad/SKILL.md",
                "---\nname: bad\n---\nmissing description\n".to_owned(),
            ),
        ])
        .await;
        let blobs = InMemoryBlobStore::new();
        let build = build_skill_catalog(
            &blobs,
            None,
            &[root_input(
                &fs,
                SkillCatalogRoot {
                    root_id: "system".to_owned(),
                    root_path: FsPath::new("/skills").unwrap(),
                    source: SkillCatalogRootSource::AttachedSnapshot {
                        snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                        attachment_path: VfsPath::parse("/skills").unwrap(),
                    },
                    trust: SkillTrustLevel::System,
                    scope: SkillScope::Global,
                },
            )],
        )
        .await
        .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 1);
        assert_eq!(build.catalog.warnings.len(), 1);
        assert!(matches!(
            build.catalog.warnings[0].kind,
            SkillLoadWarningKind::InvalidSkillDoc { .. }
        ));
    }

    #[tokio::test]
    async fn catalog_ref_stays_stable_when_only_workspace_skill_body_changes() {
        let fs = skill_fs(&[(
            "/skills/review/SKILL.md",
            format!(
                "{}\nFirst body.",
                skill_doc("review", "Use when reviewing.")
                    .trim_end_matches("Body\n")
                    .trim_end()
            ),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();
        let mut root = SkillCatalogRoot {
            root_id: "vfs".to_owned(),
            root_path: FsPath::new("/skills").unwrap(),
            source: SkillCatalogRootSource::AttachedWorkspace {
                workspace_id: VfsWorkspaceId::new("workspace-skills"),
                workspace_head_ref: BlobRef::from_bytes(b"head-1"),
                attachment_path: VfsPath::parse("/skills").unwrap(),
            },
            trust: SkillTrustLevel::User,
            scope: SkillScope::Global,
        };

        let first = build_skill_catalog(&blobs, None, &[root_input(&fs, root.clone())])
            .await
            .expect("first build");

        fs.write_file(
            &FsPath::new("/skills/review/SKILL.md").unwrap(),
            format!(
                "{}\nSecond body.",
                skill_doc("review", "Use when reviewing.")
                    .trim_end_matches("Body\n")
                    .trim_end()
            )
            .into_bytes(),
        )
        .await
        .expect("edit body");

        if let SkillCatalogRootSource::AttachedWorkspace {
            workspace_head_ref, ..
        } = &mut root.source
        {
            *workspace_head_ref = BlobRef::from_bytes(b"head-2");
        }
        let second = build_skill_catalog(&blobs, None, &[root_input(&fs, root)])
            .await
            .expect("second build");

        assert_eq!(first.catalog_ref, second.catalog_ref);
        assert!(skill_catalog_blob_refs(&second.catalog).is_empty());
        assert!(
            fs.read_file_text(&FsPath::new("/skills/review/SKILL.md").unwrap())
                .await
                .unwrap()
                .contains("Second body.")
        );
        assert_ne!(
            first.build_record.source_fingerprint.digest,
            second.build_record.source_fingerprint.digest
        );
    }

    #[tokio::test]
    async fn workspace_root_fingerprint_uses_workspace_head() {
        let fs = skill_fs(&[(
            "/workspace/.lightspeed/skills/review/SKILL.md",
            skill_doc("review", "Use when reviewing."),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();
        let root = |head: &[u8]| SkillCatalogRoot {
            root_id: "workspace".to_owned(),
            root_path: FsPath::new("/workspace/.lightspeed/skills").unwrap(),
            source: SkillCatalogRootSource::AttachedWorkspace {
                workspace_id: VfsWorkspaceId::new("workspace-1"),
                workspace_head_ref: BlobRef::from_bytes(head),
                attachment_path: VfsPath::parse("/workspace").unwrap(),
            },
            trust: SkillTrustLevel::Project,
            scope: SkillScope::Global,
        };

        let first = build_skill_catalog(&blobs, None, &[root_input(&fs, root(b"head-1"))])
            .await
            .expect("first build");
        let second = build_skill_catalog(&blobs, None, &[root_input(&fs, root(b"head-2"))])
            .await
            .expect("second build");

        assert_eq!(first.catalog_ref, second.catalog_ref);
        assert_ne!(
            first.build_record.source_fingerprint.digest,
            second.build_record.source_fingerprint.digest
        );
    }

    #[tokio::test]
    async fn prepare_publication_emits_command_when_catalog_changes() {
        let fs = skill_fs(&[(
            "/skills/review/SKILL.md",
            skill_doc("review", "Use when reviewing."),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();

        let publication = prepare_skill_catalog_publication(
            &blobs,
            None,
            None,
            &[root_input(&fs, snapshot_root("system", "/skills"))],
        )
        .await
        .expect("prepare publication");

        let Some(CoreAgentCommand::UpsertContext { key, entry, .. }) = publication.command else {
            panic!("catalog publication");
        };
        assert_eq!(key.as_str(), SKILL_CATALOG_CONTEXT_KEY);
        assert_eq!(
            entry.kind,
            engine::ContextEntryKind::Catalog {
                title: "VFS skill catalog".to_owned()
            }
        );
        assert_eq!(
            entry.provenance_ref.as_ref(),
            Some(&publication.build.catalog_ref)
        );
        assert_eq!(
            blobs.read_bytes(&entry.content.content_ref).await.unwrap(),
            format!("When a skill is relevant, read its SKILL.md through the appropriate VFS file tool before following it. VFS skill paths are not environment paths.\n\n- review ({})\n  description: Use when reviewing.\n  skill_doc_path: /skills/review/SKILL.md\n  skill_dir_path: /skills/review\n", publication.build.catalog.skills[0].skill_id).into_bytes()
        );
        assert_eq!(
            serde_json::from_slice::<SkillCatalogSnapshot>(
                &blobs
                    .read_bytes(entry.provenance_ref.as_ref().unwrap())
                    .await
                    .unwrap()
            )
            .unwrap(),
            publication.build.catalog
        );
    }

    #[tokio::test]
    async fn prepare_publication_omits_command_when_catalog_is_current() {
        let fs = skill_fs(&[(
            "/skills/review/SKILL.md",
            skill_doc("review", "Use when reviewing."),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();
        let first = prepare_skill_catalog_publication(
            &blobs,
            None,
            None,
            &[root_input(&fs, snapshot_root("system", "/skills"))],
        )
        .await
        .expect("first publication");
        let Some(CoreAgentCommand::UpsertContext { entry, .. }) = &first.command else {
            panic!("catalog publication");
        };
        let second = prepare_skill_catalog_publication(
            &blobs,
            None,
            Some(entry),
            &[root_input(&fs, snapshot_root("system", "/skills"))],
        )
        .await
        .expect("second publication");

        assert_eq!(first.build.catalog_ref, second.build.catalog_ref);
        assert_eq!(second.command, None);
    }

    fn snapshot_root(root_id: &str, root_path: &str) -> SkillCatalogRoot {
        SkillCatalogRoot {
            root_id: root_id.to_owned(),
            root_path: FsPath::new(root_path).unwrap(),
            source: SkillCatalogRootSource::AttachedSnapshot {
                snapshot_ref: BlobRef::from_bytes(format!("{root_id}:{root_path}").as_bytes()),
                attachment_path: VfsPath::parse(root_path).unwrap(),
            },
            trust: SkillTrustLevel::System,
            scope: SkillScope::Global,
        }
    }

    fn root_input<'a>(
        fs: &'a InMemoryFileSystem,
        root: SkillCatalogRoot,
    ) -> SkillCatalogRootInput<'a> {
        SkillCatalogRootInput { root, fs }
    }

    async fn skill_fs(files: &[(&str, String)]) -> InMemoryFileSystem {
        let fs = InMemoryFileSystem::full_access();
        for (path, contents) in files {
            let path = FsPath::new(*path).expect("path");
            let parent = path.parent().expect("parent");
            fs.create_directory(&parent, CreateDirectoryOptions::recursive())
                .await
                .expect("create parent");
            fs.write_file(&path, contents.clone().into_bytes())
                .await
                .expect("write skill");
        }
        fs
    }

    fn skill_doc(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n\nBody\n")
    }
}
