use engine::{BlobRef, ContextEntryKey};
use serde::{Deserialize, Serialize};
use vfs::{VfsMountAccess, VfsPath, VfsWorkspaceId};

use crate::fs::FsPath;

pub const PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX: &str = "instructions.100.prompts";
pub const PROMPT_INSTRUCTIONS_PROVIDER_KIND: &str = "lightspeed.prompts.instructions";
pub const PROMPT_INSTRUCTIONS_REPORT_SCHEMA_VERSION: &str =
    "lightspeed.prompts.instructions.report.v1";
pub const PROMPT_SOURCE_FINGERPRINT_SCHEMA_VERSION: &str =
    "lightspeed.prompts.instructions.source_fingerprint.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptInstructionsReport {
    pub schema_version: String,
    pub source_fingerprint: PromptSourceFingerprint,
    pub total_chars: u32,
    pub total_bytes: u64,
    pub sources: Vec<PromptSourceReport>,
    pub warnings: Vec<PromptWarning>,
}

impl PromptInstructionsReport {
    pub fn new(
        source_fingerprint: PromptSourceFingerprint,
        total_chars: u32,
        total_bytes: u64,
        sources: Vec<PromptSourceReport>,
        warnings: Vec<PromptWarning>,
    ) -> Self {
        Self {
            schema_version: PROMPT_INSTRUCTIONS_REPORT_SCHEMA_VERSION.to_owned(),
            source_fingerprint,
            total_chars,
            total_bytes,
            sources,
            warnings,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSourceFingerprint {
    pub algorithm: String,
    pub digest: String,
    pub inputs: Vec<PromptSourceFingerprintInput>,
}

impl PromptSourceFingerprint {
    pub fn sha256(digest: BlobRef, inputs: Vec<PromptSourceFingerprintInput>) -> Self {
        Self {
            algorithm: "sha256".to_owned(),
            digest: digest.to_string(),
            inputs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptSourceFingerprintInput {
    SnapshotRoot {
        root_id: String,
        snapshot_ref: BlobRef,
        root_path: VfsPath,
    },
    WorkspaceRoot {
        root_id: String,
        workspace_id: VfsWorkspaceId,
        workspace_head_ref: BlobRef,
        workspace_revision: u64,
        root_path: VfsPath,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSourceReport {
    pub id: String,
    pub root_id: String,
    pub path: String,
    pub published: bool,
    pub context_key: Option<ContextEntryKey>,
    pub source: PromptSourceLocation,
    pub content_ref: BlobRef,
    pub chars: u32,
    pub bytes: u64,
    pub sha256: String,
    pub truncated: bool,
    pub writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptSourceLocation {
    MountedSnapshot {
        source_snapshot_ref: BlobRef,
        source_mount_path: VfsPath,
        prompt_file_path: VfsPath,
    },
    MountedWorkspace {
        workspace_id: VfsWorkspaceId,
        workspace_revision: u64,
        workspace_head_ref: BlobRef,
        source_mount_path: VfsPath,
        prompt_file_path: VfsPath,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptWarning {
    pub root_id: String,
    pub path: Option<String>,
    pub kind: PromptWarningKind,
}

impl PromptWarning {
    pub fn new(root_id: impl Into<String>, path: Option<String>, kind: PromptWarningKind) -> Self {
        Self {
            root_id: root_id.into(),
            path,
            kind,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptWarningKind {
    Filesystem { message: String },
    InvalidPath { message: String },
    InvalidUtf8 { message: String },
    SourceTruncated { max_chars: u32 },
    TotalLimitReached { max_chars: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptAssemblyLimits {
    pub max_source_chars: u32,
    pub max_total_chars: u32,
}

impl Default for PromptAssemblyLimits {
    fn default() -> Self {
        Self {
            max_source_chars: 64 * 1024,
            max_total_chars: 256 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptRoot {
    pub root_id: String,
    pub root_path: FsPath,
    pub source: PromptRootSource,
    pub access: VfsMountAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptRootSource {
    MountedSnapshot {
        snapshot_ref: BlobRef,
        mount_path: VfsPath,
    },
    MountedWorkspace {
        workspace_id: VfsWorkspaceId,
        workspace_head_ref: BlobRef,
        workspace_revision: u64,
        mount_path: VfsPath,
    },
}
