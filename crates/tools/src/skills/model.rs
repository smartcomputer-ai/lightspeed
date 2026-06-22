use engine::{BlobRef, SkillId, ToolExecutionTarget};
use serde::{Deserialize, Serialize};
use vfs::{VfsPath, VfsWorkspaceId};

use crate::fs::FsPath;

pub const SKILL_CATALOG_SCHEMA_VERSION: &str = "lightspeed.skills.catalog.v1";
pub const SKILL_CATALOG_BUILD_SCHEMA_VERSION: &str = "lightspeed.skills.catalog.build.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogSnapshot {
    pub schema_version: String,
    pub target: Option<ToolExecutionTarget>,
    pub skills: Vec<SkillMetadata>,
    pub warnings: Vec<SkillLoadWarning>,
}

impl SkillCatalogSnapshot {
    pub fn new(
        target: Option<ToolExecutionTarget>,
        skills: Vec<SkillMetadata>,
        warnings: Vec<SkillLoadWarning>,
    ) -> Self {
        Self {
            schema_version: SKILL_CATALOG_SCHEMA_VERSION.to_owned(),
            target,
            skills,
            warnings,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogBuildRecord {
    pub schema_version: String,
    pub catalog_ref: BlobRef,
    pub source_fingerprint: SkillCatalogSourceFingerprint,
}

impl SkillCatalogBuildRecord {
    pub fn new(catalog_ref: BlobRef, source_fingerprint: SkillCatalogSourceFingerprint) -> Self {
        Self {
            schema_version: SKILL_CATALOG_BUILD_SCHEMA_VERSION.to_owned(),
            catalog_ref,
            source_fingerprint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogSourceFingerprint {
    pub algorithm: String,
    pub digest: String,
    pub inputs: Vec<SkillCatalogSourceInput>,
}

impl SkillCatalogSourceFingerprint {
    pub fn sha256(digest: BlobRef, inputs: Vec<SkillCatalogSourceInput>) -> Self {
        Self {
            algorithm: "sha256".to_owned(),
            digest: digest.to_string(),
            inputs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillCatalogSourceInput {
    SnapshotRoot {
        root_id: String,
        snapshot_ref: BlobRef,
        root_path: VfsPath,
    },
    WorkspaceRoot {
        root_id: String,
        workspace_id: VfsWorkspaceId,
        workspace_head_ref: BlobRef,
        root_path: VfsPath,
    },
    HostRoot {
        root_id: String,
        target: ToolExecutionTarget,
        root_path: String,
        root_fingerprint: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub source: SkillSource,
    pub scope: SkillScope,
    pub target: Option<ToolExecutionTarget>,
    pub enabled: bool,
    pub trust: SkillTrustLevel,
    pub interface: Option<SkillInterface>,
    pub dependencies: SkillDependencies,
    pub location: SkillLocation,
    pub skill_doc_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillSource {
    Snapshot {
        root_id: String,
        snapshot_ref: BlobRef,
    },
    Workspace {
        root_id: String,
        workspace_id: VfsWorkspaceId,
    },
    HostPath {
        root_id: String,
        target: ToolExecutionTarget,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    #[default]
    Global,
    Target,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTrustLevel {
    System,
    Organization,
    User,
    Project,
    Host,
    Remote,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInterface {
    pub display_name: Option<String>,
    pub short_description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDependencies {
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillLocation {
    MountedSnapshot {
        source_snapshot_ref: BlobRef,
        source_mount_path: VfsPath,
        skill_dir_path: VfsPath,
        skill_doc_path: VfsPath,
    },
    MountedWorkspace {
        workspace_id: VfsWorkspaceId,
        source_mount_path: VfsPath,
        skill_dir_path: VfsPath,
        skill_doc_path: VfsPath,
    },
    HostFilesystem {
        target: ToolExecutionTarget,
        root_path: String,
        skill_dir_path: String,
        skill_doc_path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillLoadWarning {
    pub root_id: String,
    pub path: Option<String>,
    pub kind: SkillLoadWarningKind,
}

impl SkillLoadWarning {
    pub fn new(
        root_id: impl Into<String>,
        path: Option<String>,
        kind: SkillLoadWarningKind,
    ) -> Self {
        Self {
            root_id: root_id.into(),
            path,
            kind,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillLoadWarningKind {
    MissingSkillDoc,
    InvalidSkillDoc { message: String },
    Filesystem { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCatalogRoot {
    pub root_id: String,
    pub root_path: FsPath,
    pub source: SkillCatalogRootSource,
    pub trust: SkillTrustLevel,
    pub scope: SkillScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillCatalogRootSource {
    MountedSnapshot {
        snapshot_ref: BlobRef,
        mount_path: VfsPath,
    },
    MountedWorkspace {
        workspace_id: VfsWorkspaceId,
        workspace_head_ref: BlobRef,
        mount_path: VfsPath,
    },
    HostFilesystem {
        target: ToolExecutionTarget,
    },
}
