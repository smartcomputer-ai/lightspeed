use super::SkillId;
use engine::BlobRef;
use serde::{Deserialize, Serialize};
use vfs::{VfsPath, VfsWorkspaceId};

use crate::fs::FsPath;

pub const SKILL_CATALOG_SCHEMA_VERSION: &str = "lightspeed.skills.catalog.v2";
pub const SKILL_CATALOG_BUILD_SCHEMA_VERSION: &str = "lightspeed.skills.catalog.build.v1";
pub const VFS_SKILL_CATALOG_ID: &str = "vfs";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillCatalogSource {
    Vfs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogSnapshot {
    pub schema_version: String,
    pub catalog_id: String,
    pub source: SkillCatalogSource,
    pub skills: Vec<SkillMetadata>,
    pub warnings: Vec<SkillLoadWarning>,
}

impl SkillCatalogSnapshot {
    pub fn new(skills: Vec<SkillMetadata>, warnings: Vec<SkillLoadWarning>) -> Self {
        Self {
            schema_version: SKILL_CATALOG_SCHEMA_VERSION.to_owned(),
            catalog_id: VFS_SKILL_CATALOG_ID.to_owned(),
            source: SkillCatalogSource::Vfs,
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
    /// `digest` is the content hash of the canonical inputs. Only its hex is
    /// stored: the fingerprint names no blob, and a `sha256:`-shaped string
    /// would read as a blob ref to the collector.
    pub fn sha256(digest: BlobRef, inputs: Vec<SkillCatalogSourceInput>) -> Self {
        Self {
            algorithm: "sha256".to_owned(),
            digest: crate::prompts::model::digest_hex(&digest),
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub source: SkillSource,
    pub scope: SkillScope,
    pub enabled: bool,
    pub trust: SkillTrustLevel,
    pub interface: Option<SkillInterface>,
    pub dependencies: SkillDependencies,
    pub location: SkillLocation,
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

// Preserve the serialized names used by stored source reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillLocation {
    #[serde(rename = "linked_snapshot")]
    AttachedSnapshot {
        source_snapshot_ref: BlobRef,
        #[serde(rename = "source_link_path")]
        source_attachment_path: VfsPath,
        skill_dir_path: VfsPath,
        skill_doc_path: VfsPath,
    },
    #[serde(rename = "linked_workspace")]
    AttachedWorkspace {
        workspace_id: VfsWorkspaceId,
        #[serde(rename = "source_link_path")]
        source_attachment_path: VfsPath,
        skill_dir_path: VfsPath,
        skill_doc_path: VfsPath,
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
    UnavailableWorkspaceAttachment { reason: String },
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
    AttachedSnapshot {
        snapshot_ref: BlobRef,
        attachment_path: VfsPath,
    },
    AttachedWorkspace {
        workspace_id: VfsWorkspaceId,
        workspace_head_ref: BlobRef,
        attachment_path: VfsPath,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stored_attachment_sources_keep_their_serialized_format() {
        let snapshot_ref = format!("sha256:{}", "a".repeat(64));
        let sources = [
            json!({
                "type": "linked_snapshot",
                "source_snapshot_ref": snapshot_ref,
                "source_link_path": "/workspace",
                "skill_dir_path": "/workspace/skills/check", "skill_doc_path": "/workspace/skills/check/SKILL.md"
            }),
            json!({
                "type": "linked_workspace",
                "workspace_id": "workspace_1",
                "source_link_path": "/workspace",
                "skill_dir_path": "/workspace/skills/check", "skill_doc_path": "/workspace/skills/check/SKILL.md"
            }),
        ];
        for source in sources {
            let decoded: SkillLocation =
                serde_json::from_value(source.clone()).expect("read stored source");
            assert_eq!(serde_json::to_value(decoded).expect("write source"), source);
        }
    }
}
