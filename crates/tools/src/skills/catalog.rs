use std::cmp::Ordering;

use engine::{
    BlobRef, CoreAgentCommand, CoreAgentState, SkillCatalogContext, SkillId, ToolExecutionTarget,
    storage::{BlobStore, BlobStoreError},
};
use serde::Serialize;
use thiserror::Error;
use vfs::VfsPath;

use crate::{
    host::fs::{FileSystem, FsError, FsPath},
    skills::{
        SkillCatalogBuildRecord, SkillCatalogRoot, SkillCatalogRootSource, SkillCatalogSnapshot,
        SkillCatalogSourceFingerprint, SkillCatalogSourceInput, SkillDependencies, SkillInterface,
        SkillLoadWarning, SkillLoadWarningKind, SkillLocation, SkillMetadata, SkillSource,
        parse_skill_frontmatter,
    },
};

const SOURCE_FINGERPRINT_SCHEMA_VERSION: &str = "forge.skills.catalog.source_fingerprint.v1";
const HOST_ROOT_FINGERPRINT_SCHEMA_VERSION: &str = "forge.skills.catalog.host_root.v1";
const PARSER_VERSION: &str = "forge.skills.frontmatter_parser.v1";

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
    target: Option<ToolExecutionTarget>,
    roots: Vec<SkillCatalogRootInput<'a>>,
}

impl<'a> SkillCatalogBuilder<'a> {
    pub fn new(blobs: &'a dyn BlobStore) -> Self {
        Self {
            blobs,
            target: None,
            roots: Vec::new(),
        }
    }

    pub fn with_target(mut self, target: Option<ToolExecutionTarget>) -> Self {
        self.target = target;
        self
    }

    pub fn with_root(mut self, root: SkillCatalogRootInput<'a>) -> Self {
        self.roots.push(root);
        self
    }

    pub async fn build(self) -> Result<SkillCatalogBuild, SkillCatalogError> {
        build_skill_catalog(self.blobs, self.target, &self.roots).await
    }
}

pub async fn build_skill_catalog(
    blobs: &dyn BlobStore,
    target: Option<ToolExecutionTarget>,
    roots: &[SkillCatalogRootInput<'_>],
) -> Result<SkillCatalogBuild, SkillCatalogError> {
    let mut sorted_roots = roots.iter().collect::<Vec<_>>();
    sorted_roots.sort_by(compare_roots);

    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    let mut source_inputs = Vec::new();

    for input in sorted_roots {
        let scan = scan_root(input).await;
        skills.extend(scan.skills);
        warnings.extend(scan.warnings);
        source_inputs.push(scan.source_input);
    }

    skills.sort_by(|left, right| left.skill_id.as_str().cmp(right.skill_id.as_str()));
    warnings.sort_by(compare_warnings);
    source_inputs.sort_by(compare_source_inputs);

    let catalog = SkillCatalogSnapshot::new(target, skills, warnings);
    let catalog_bytes = encode_json(&catalog)?;
    let catalog_ref = blobs.put_bytes(catalog_bytes.clone()).await?;
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
    state: &CoreAgentState,
    target: Option<ToolExecutionTarget>,
    roots: &[SkillCatalogRootInput<'_>],
) -> Result<SkillCatalogPublication, SkillCatalogError> {
    let build = build_skill_catalog(blobs, target, roots).await?;
    let command = if state
        .skills
        .catalog
        .as_ref()
        .is_some_and(|catalog| catalog.catalog_ref == build.catalog_ref)
    {
        None
    } else {
        Some(CoreAgentCommand::SetSkillCatalog {
            catalog: Some(SkillCatalogContext {
                catalog_ref: build.catalog_ref.clone(),
            }),
        })
    };

    Ok(SkillCatalogPublication { build, command })
}

async fn scan_root(input: &SkillCatalogRootInput<'_>) -> RootScanResult {
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
            scan.record_host_observation(HostRootObservation::root_error(error.to_string()));
            return scan.finish(input);
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
                scan.record_host_observation(HostRootObservation::path_error(
                    path,
                    error.to_string(),
                ));
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
                scan.record_host_observation(HostRootObservation::path_error(
                    skill_dir_path.as_str().to_owned(),
                    error.to_string(),
                ));
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
                scan.record_host_observation(HostRootObservation::missing(
                    skill_doc_path.as_str().to_owned(),
                ));
                continue;
            }
            Err(error) => {
                scan.warn(
                    Some(skill_doc_path.as_str().to_owned()),
                    SkillLoadWarningKind::Filesystem {
                        message: error.to_string(),
                    },
                );
                scan.record_host_observation(HostRootObservation::path_error(
                    skill_doc_path.as_str().to_owned(),
                    error.to_string(),
                ));
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
                scan.record_host_observation(HostRootObservation::invalid(
                    skill_doc_path.as_str().to_owned(),
                    error.to_string(),
                ));
                continue;
            }
        };

        let frontmatter_ref = BlobRef::from_bytes(frontmatter.raw_frontmatter.as_bytes());
        scan.record_host_observation(HostRootObservation::valid(
            skill_doc_path.as_str().to_owned(),
            frontmatter_ref,
        ));
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

    scan.finish(input)
}

fn metadata_for_skill(
    input: &SkillCatalogRootInput<'_>,
    skill_dir_path: &FsPath,
    skill_doc_path: &FsPath,
    frontmatter: crate::skills::SkillFrontmatter,
) -> Result<SkillMetadata, SkillCatalogError> {
    let skill_id = skill_id_for_path(&input.root, skill_doc_path);
    let target = match &input.root.source {
        SkillCatalogRootSource::HostFilesystem { target } => Some(target.clone()),
        SkillCatalogRootSource::MountedSnapshot { .. }
        | SkillCatalogRootSource::MountedWorkspace { .. } => None,
    };
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
        target,
        enabled: true,
        trust: input.root.trust,
        interface: Some(SkillInterface {
            display_name: None,
            short_description,
        }),
        dependencies: SkillDependencies::default(),
        location,
        skill_doc_ref: None,
    })
}

fn source_for_root(root: &SkillCatalogRoot) -> SkillSource {
    match &root.source {
        SkillCatalogRootSource::MountedSnapshot { snapshot_ref, .. } => SkillSource::Snapshot {
            root_id: root.root_id.clone(),
            snapshot_ref: snapshot_ref.clone(),
        },
        SkillCatalogRootSource::MountedWorkspace { workspace_id, .. } => SkillSource::Workspace {
            root_id: root.root_id.clone(),
            workspace_id: workspace_id.clone(),
        },
        SkillCatalogRootSource::HostFilesystem { target } => SkillSource::HostPath {
            root_id: root.root_id.clone(),
            target: target.clone(),
        },
    }
}

fn location_for_skill(
    root: &SkillCatalogRoot,
    skill_dir_path: &FsPath,
    skill_doc_path: &FsPath,
) -> Result<SkillLocation, SkillCatalogError> {
    match &root.source {
        SkillCatalogRootSource::MountedSnapshot {
            snapshot_ref,
            mount_path,
        } => Ok(SkillLocation::MountedSnapshot {
            source_snapshot_ref: snapshot_ref.clone(),
            source_mount_path: mount_path.clone(),
            skill_dir_path: vfs_path(skill_dir_path)?,
            skill_doc_path: vfs_path(skill_doc_path)?,
        }),
        SkillCatalogRootSource::MountedWorkspace {
            workspace_id,
            workspace_head_ref: _,
            mount_path,
        } => Ok(SkillLocation::MountedWorkspace {
            workspace_id: workspace_id.clone(),
            source_mount_path: mount_path.clone(),
            skill_dir_path: vfs_path(skill_dir_path)?,
            skill_doc_path: vfs_path(skill_doc_path)?,
        }),
        SkillCatalogRootSource::HostFilesystem { target } => Ok(SkillLocation::HostFilesystem {
            target: target.clone(),
            root_path: root.root_path.as_str().to_owned(),
            skill_dir_path: skill_dir_path.as_str().to_owned(),
            skill_doc_path: skill_doc_path.as_str().to_owned(),
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
        SkillCatalogRootSource::MountedSnapshot { snapshot_ref, .. } => {
            format!("snapshot:{snapshot_ref}")
        }
        SkillCatalogRootSource::MountedWorkspace {
            workspace_id,
            workspace_head_ref: _,
            mount_path: _,
        } => format!("workspace:{workspace_id}"),
        SkillCatalogRootSource::HostFilesystem { target } => {
            format!("host:{}:{}", target.namespace, target.id)
        }
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
    host_observations: Vec<HostRootObservation>,
}

impl RootScan {
    fn new(input: &SkillCatalogRootInput<'_>) -> Self {
        Self {
            root_id: input.root.root_id.clone(),
            skills: Vec::new(),
            warnings: Vec::new(),
            host_observations: Vec::new(),
        }
    }

    fn warn(&mut self, path: Option<String>, kind: SkillLoadWarningKind) {
        self.warnings
            .push(SkillLoadWarning::new(self.root_id.clone(), path, kind));
    }

    fn record_host_observation(&mut self, observation: HostRootObservation) {
        self.host_observations.push(observation);
    }

    fn finish(mut self, input: &SkillCatalogRootInput<'_>) -> RootScanResult {
        self.host_observations
            .sort_by(|left, right| left.path.cmp(&right.path));
        let source_input = source_input_for_root(input, &self.host_observations);
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

fn source_input_for_root(
    input: &SkillCatalogRootInput<'_>,
    host_observations: &[HostRootObservation],
) -> SkillCatalogSourceInput {
    match &input.root.source {
        SkillCatalogRootSource::MountedSnapshot { snapshot_ref, .. } => {
            SkillCatalogSourceInput::SnapshotRoot {
                root_id: input.root.root_id.clone(),
                snapshot_ref: snapshot_ref.clone(),
                root_path: vfs_path(&input.root.root_path).unwrap_or_else(|_| VfsPath::root()),
            }
        }
        SkillCatalogRootSource::MountedWorkspace {
            workspace_id,
            workspace_head_ref,
            mount_path: _,
        } => SkillCatalogSourceInput::WorkspaceRoot {
            root_id: input.root.root_id.clone(),
            workspace_id: workspace_id.clone(),
            workspace_head_ref: workspace_head_ref.clone(),
            root_path: vfs_path(&input.root.root_path).unwrap_or_else(|_| VfsPath::root()),
        },
        SkillCatalogRootSource::HostFilesystem { target } => {
            let fingerprint = host_root_fingerprint(input, host_observations);
            SkillCatalogSourceInput::HostRoot {
                root_id: input.root.root_id.clone(),
                target: target.clone(),
                root_path: input.root.root_path.as_str().to_owned(),
                root_fingerprint: fingerprint,
            }
        }
    }
}

fn host_root_fingerprint(
    input: &SkillCatalogRootInput<'_>,
    observations: &[HostRootObservation],
) -> String {
    let payload = HostRootFingerprintPayload {
        schema_version: HOST_ROOT_FINGERPRINT_SCHEMA_VERSION,
        parser_version: PARSER_VERSION,
        root_id: &input.root.root_id,
        root_path: input.root.root_path.as_str(),
        target: match &input.root.source {
            SkillCatalogRootSource::HostFilesystem { target } => Some(target),
            SkillCatalogRootSource::MountedSnapshot { .. }
            | SkillCatalogRootSource::MountedWorkspace { .. } => None,
        },
        observations,
    };
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    BlobRef::from_bytes(&bytes).to_string()
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
        | SkillCatalogSourceInput::WorkspaceRoot { root_id, .. }
        | SkillCatalogSourceInput::HostRoot { root_id, .. } => root_id.clone(),
    }
}

#[derive(Serialize)]
struct SourceFingerprintPayload<'a> {
    schema_version: &'static str,
    parser_version: &'static str,
    inputs: &'a [SkillCatalogSourceInput],
}

#[derive(Serialize)]
struct HostRootFingerprintPayload<'a> {
    schema_version: &'static str,
    parser_version: &'static str,
    root_id: &'a str,
    root_path: &'a str,
    target: Option<&'a ToolExecutionTarget>,
    observations: &'a [HostRootObservation],
}

#[derive(Clone, Debug, Serialize)]
struct HostRootObservation {
    path: String,
    status: HostRootObservationStatus,
}

impl HostRootObservation {
    fn valid(path: String, frontmatter_ref: BlobRef) -> Self {
        Self {
            path,
            status: HostRootObservationStatus::Valid { frontmatter_ref },
        }
    }

    fn missing(path: String) -> Self {
        Self {
            path,
            status: HostRootObservationStatus::MissingSkillDoc,
        }
    }

    fn invalid(path: String, message: String) -> Self {
        Self {
            path,
            status: HostRootObservationStatus::InvalidSkillDoc { message },
        }
    }

    fn path_error(path: String, message: String) -> Self {
        Self {
            path,
            status: HostRootObservationStatus::Filesystem { message },
        }
    }

    fn root_error(message: String) -> Self {
        Self {
            path: ".".to_owned(),
            status: HostRootObservationStatus::Filesystem { message },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostRootObservationStatus {
    Valid { frontmatter_ref: BlobRef },
    MissingSkillDoc,
    InvalidSkillDoc { message: String },
    Filesystem { message: String },
}

trait RootReadDirectory {
    async fn fs_read_directory(
        &self,
        fs: &dyn FileSystem,
    ) -> Result<Vec<crate::host::fs::ReadDirectoryEntry>, FsError>;
}

impl RootReadDirectory for SkillCatalogRoot {
    async fn fs_read_directory(
        &self,
        fs: &dyn FileSystem,
    ) -> Result<Vec<crate::host::fs::ReadDirectoryEntry>, FsError> {
        fs.read_directory(&self.root_path).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        BlobRef, ToolExecutionTarget,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use vfs::VfsWorkspaceId;

    use super::*;
    use crate::{
        host::fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
        skills::{SkillScope, SkillTrustLevel},
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
                    source: SkillCatalogRootSource::MountedSnapshot {
                        snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                        mount_path: VfsPath::parse("/skills").unwrap(),
                    },
                    trust: SkillTrustLevel::System,
                    scope: SkillScope::Global,
                },
            )],
        )
        .await
        .expect("build catalog");

        assert_eq!(build.catalog.skills.len(), 2);
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
            SkillLocation::MountedSnapshot { .. }
        ));
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
                        source: SkillCatalogRootSource::MountedSnapshot {
                            snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                            mount_path: VfsPath::parse("/skills-one").unwrap(),
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
                        source: SkillCatalogRootSource::MountedSnapshot {
                            snapshot_ref: BlobRef::from_bytes(b"snapshot-2"),
                            mount_path: VfsPath::parse("/skills-two").unwrap(),
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
                    source: SkillCatalogRootSource::MountedSnapshot {
                        snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                        mount_path: VfsPath::parse("/skills").unwrap(),
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
    async fn same_semantic_catalog_has_same_ref_when_body_changes_outside_frontmatter() {
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
        let root = SkillCatalogRoot {
            root_id: "host".to_owned(),
            root_path: FsPath::new("/skills").unwrap(),
            source: SkillCatalogRootSource::HostFilesystem {
                target: ToolExecutionTarget::new("host", "vm-1"),
            },
            trust: SkillTrustLevel::Host,
            scope: SkillScope::Target,
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

        let second = build_skill_catalog(&blobs, None, &[root_input(&fs, root)])
            .await
            .expect("second build");

        assert_eq!(first.catalog_ref, second.catalog_ref);
        assert_eq!(
            first.build_record.source_fingerprint.digest,
            second.build_record.source_fingerprint.digest
        );
    }

    #[tokio::test]
    async fn host_fingerprint_and_catalog_ref_change_when_catalog_metadata_changes() {
        let fs = skill_fs(&[(
            "/skills/review/SKILL.md",
            skill_doc("review", "Use when reviewing."),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();
        let root = SkillCatalogRoot {
            root_id: "host".to_owned(),
            root_path: FsPath::new("/skills").unwrap(),
            source: SkillCatalogRootSource::HostFilesystem {
                target: ToolExecutionTarget::new("host", "vm-1"),
            },
            trust: SkillTrustLevel::Host,
            scope: SkillScope::Target,
        };

        let first = build_skill_catalog(&blobs, None, &[root_input(&fs, root.clone())])
            .await
            .expect("first build");
        fs.write_file(
            &FsPath::new("/skills/review/SKILL.md").unwrap(),
            skill_doc("review", "Use when reviewing changed.").into_bytes(),
        )
        .await
        .expect("edit metadata");
        let second = build_skill_catalog(&blobs, None, &[root_input(&fs, root)])
            .await
            .expect("second build");

        assert_ne!(first.catalog_ref, second.catalog_ref);
        assert_ne!(
            first.build_record.source_fingerprint.digest,
            second.build_record.source_fingerprint.digest
        );
    }

    #[tokio::test]
    async fn workspace_root_fingerprint_uses_workspace_head() {
        let fs = skill_fs(&[(
            "/workspace/.forge/skills/review/SKILL.md",
            skill_doc("review", "Use when reviewing."),
        )])
        .await;
        let blobs = InMemoryBlobStore::new();
        let root = |head: &[u8]| SkillCatalogRoot {
            root_id: "workspace".to_owned(),
            root_path: FsPath::new("/workspace/.forge/skills").unwrap(),
            source: SkillCatalogRootSource::MountedWorkspace {
                workspace_id: VfsWorkspaceId::new("workspace-1"),
                workspace_head_ref: BlobRef::from_bytes(head),
                mount_path: VfsPath::parse("/workspace").unwrap(),
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
        let state = CoreAgentState::new();

        let publication = prepare_skill_catalog_publication(
            &blobs,
            &state,
            None,
            &[root_input(&fs, snapshot_root("system", "/skills"))],
        )
        .await
        .expect("prepare publication");

        assert_eq!(
            publication.command,
            Some(CoreAgentCommand::SetSkillCatalog {
                catalog: Some(SkillCatalogContext {
                    catalog_ref: publication.build.catalog_ref.clone(),
                }),
            })
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
            &CoreAgentState::new(),
            None,
            &[root_input(&fs, snapshot_root("system", "/skills"))],
        )
        .await
        .expect("first publication");
        let mut state = CoreAgentState::new();
        state.skills.catalog = Some(SkillCatalogContext {
            catalog_ref: first.build.catalog_ref.clone(),
        });

        let second = prepare_skill_catalog_publication(
            &blobs,
            &state,
            None,
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
            source: SkillCatalogRootSource::MountedSnapshot {
                snapshot_ref: BlobRef::from_bytes(format!("{root_id}:{root_path}").as_bytes()),
                mount_path: VfsPath::parse(root_path).unwrap(),
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
