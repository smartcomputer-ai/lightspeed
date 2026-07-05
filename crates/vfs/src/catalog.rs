use async_trait::async_trait;
use engine::{BlobRef, SessionId, StringIdError, validate_general_string_id};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fmt, str::FromStr};
use thiserror::Error;

use crate::manifest::VfsTotals;
use crate::path::VfsPath;

macro_rules! vfs_string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                Self::try_new(value)
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", stringify!($name)))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, StringIdError> {
                let value = value.into();
                validate_general_string_id(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, StringIdError> {
                Self::try_new(value)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = StringIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StringIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl FromStr for $name {
            type Err = StringIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

vfs_string_id!(VfsWorkspaceId);

#[derive(Debug, Error)]
pub enum VfsCatalogError {
    #[error("vfs catalog {kind} already exists: {id}")]
    AlreadyExists { kind: &'static str, id: String },

    #[error("vfs catalog {kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error(
        "vfs workspace revision conflict for {workspace_id}: expected {expected_revision}, actual {actual_revision}"
    )]
    RevisionConflict {
        workspace_id: VfsWorkspaceId,
        expected_revision: u64,
        actual_revision: u64,
    },

    #[error("invalid vfs catalog request: {message}")]
    InvalidInput { message: String },

    #[error("vfs catalog store failure: {message}")]
    Store { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsSnapshotRecord {
    pub snapshot_ref: BlobRef,
    pub source: VfsSnapshotSource,
    pub display_name: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsSnapshotSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_ref: Option<BlobRef>,
}

impl VfsSnapshotSource {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            subject: None,
            metadata_ref: None,
        }
    }

    pub fn unknown() -> Self {
        Self::new("unknown")
    }

    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn with_metadata_ref(mut self, metadata_ref: BlobRef) -> Self {
        self.metadata_ref = Some(metadata_ref);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsWorkspaceRecord {
    pub workspace_id: VfsWorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub base_snapshot_ref: Option<BlobRef>,
    pub head_snapshot_ref: BlobRef,
    /// Totals of the head snapshot's manifest, denormalized so reads and
    /// listings never dereference the manifest blob.
    pub head_totals: VfsTotals,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVfsWorkspaceRecord {
    pub workspace_id: VfsWorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub base_snapshot_ref: Option<BlobRef>,
    pub head_snapshot_ref: BlobRef,
    pub head_totals: VfsTotals,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareAndSetVfsWorkspaceHead {
    pub workspace_id: VfsWorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// When present, replaces the workspace display name alongside the head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub new_head_snapshot_ref: BlobRef,
    pub new_head_totals: VfsTotals,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsMountRecord {
    pub session_id: SessionId,
    pub mount_path: VfsPath,
    pub source: VfsMountSource,
    pub access: VfsMountAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VfsMountSource {
    Snapshot { snapshot_ref: BlobRef },
    Workspace { workspace_id: VfsWorkspaceId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VfsMountAccess {
    ReadOnly,
    ReadWrite,
}

impl VfsMountAccess {
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsMountTable {
    pub session_id: SessionId,
    pub mounts: Vec<VfsMountRecord>,
}

#[async_trait]
pub trait VfsSnapshotStore: Send + Sync {
    async fn record_snapshot(&self, record: VfsSnapshotRecord) -> Result<(), VfsCatalogError>;

    async fn read_snapshot(
        &self,
        snapshot_ref: &BlobRef,
    ) -> Result<VfsSnapshotRecord, VfsCatalogError>;
}

#[async_trait]
pub trait VfsWorkspaceStore: Send + Sync {
    async fn create_workspace(
        &self,
        record: CreateVfsWorkspaceRecord,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError>;

    async fn read_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError>;

    /// Enumerate every workspace in the store, most recently updated first.
    async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError>;

    async fn compare_and_set_head(
        &self,
        request: CompareAndSetVfsWorkspaceHead,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError>;

    async fn delete_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError>;
}

#[async_trait]
pub trait VfsMountStore: Send + Sync {
    async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError>;

    async fn list_mounts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<VfsMountRecord>, VfsCatalogError>;

    async fn remove_mount(
        &self,
        session_id: &SessionId,
        mount_path: &VfsPath,
    ) -> Result<(), VfsCatalogError>;
}

pub trait VfsCatalogStore: VfsSnapshotStore + VfsWorkspaceStore + VfsMountStore {}

impl<T> VfsCatalogStore for T where T: VfsSnapshotStore + VfsWorkspaceStore + VfsMountStore {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfs_workspace_id_uses_engine_string_id_rules() {
        let id = VfsWorkspaceId::new("workspace-1");

        assert_eq!(id.as_str(), "workspace-1");
        assert!(VfsWorkspaceId::try_new("-workspace").is_err());
        assert!(VfsWorkspaceId::try_new("workspace/path").is_err());
        assert!(VfsWorkspaceId::try_new("").is_err());
    }

    #[test]
    fn catalog_records_round_trip_as_json() {
        let snapshot_ref = BlobRef::from_bytes(b"manifest");
        let workspace_id = VfsWorkspaceId::new("workspace-1");

        let snapshot = VfsSnapshotRecord {
            snapshot_ref: snapshot_ref.clone(),
            source: VfsSnapshotSource::new("skill")
                .with_subject("openai-docs")
                .with_metadata_ref(BlobRef::from_bytes(b"skill metadata")),
            display_name: Some("OpenAI Docs".to_string()),
            created_at_ms: 10,
        };
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: VfsSnapshotRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, snapshot);

        let workspace = VfsWorkspaceRecord {
            workspace_id: workspace_id.clone(),
            display_name: Some("Scratch".to_string()),
            base_snapshot_ref: Some(snapshot_ref.clone()),
            head_snapshot_ref: BlobRef::from_bytes(b"head"),
            head_totals: VfsTotals { files: 3, bytes: 42 },
            revision: 7,
            created_at_ms: 11,
            updated_at_ms: 12,
        };
        let encoded = serde_json::to_string(&workspace).unwrap();
        let decoded: VfsWorkspaceRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, workspace);

        let mount = VfsMountRecord {
            session_id: SessionId::new("session-1"),
            mount_path: VfsPath::parse("/workspace").unwrap(),
            source: VfsMountSource::Workspace { workspace_id },
            access: VfsMountAccess::ReadWrite,
        };
        assert!(mount.access.is_writable());
        let encoded = serde_json::to_string(&mount).unwrap();
        let decoded: VfsMountRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, mount);
    }
}
