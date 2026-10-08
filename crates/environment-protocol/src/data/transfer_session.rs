//! One logical transfer, many bounded exchanges. No storage or VFS identities cross this boundary.
use super::{
    inventory::{InventoryEntry, InventoryLimits},
    transfer::TransferOnExisting,
};
use crate::shared::{ByteChunk, EnvironmentPath};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "direction",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransferSelection {
    Capture {
        source: EnvironmentPath,
    },
    Materialize {
        destination: EnvironmentPath,
        #[serde(default)]
        on_existing: TransferOnExisting,
    },
    /// Write exactly one file, rejecting directories and preserving existing
    /// file permissions. Uses the same bounded chunks and durable receipts.
    /// A separate direction makes older endpoints reject unsupported semantics.
    WriteFile {
        destination: EnvironmentPath,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransferRequest {
    /// Idempotent until operation expiry, including completed receipts after daemon restart.
    /// IDs must not be reused for different input.
    Begin {
        operation_id: String,
        selection: TransferSelection,
        #[serde(default)]
        limits: InventoryLimits,
    },
    /// Advances bounded scan/staging work. Inspect status until ready, then commit.
    Advance {
        operation_id: String,
    },
    Inventory {
        operation_id: String,
        offset: u32,
    },
    /// Parent-before-child inventory pages. Retrying the identical page is safe.
    Append {
        operation_id: String,
        offset: u32,
        entries: Vec<InventoryEntry>,
        last: bool,
    },
    Missing {
        operation_id: String,
        offset: u32,
    },
    Read {
        operation_id: String,
        digest: String,
        offset: u64,
    },
    /// Sequential chunks; an identical chunk may be retried. Only complete verified files are reusable.
    Write {
        operation_id: String,
        digest: String,
        offset: u64,
        data: ByteChunk,
    },
    Commit {
        operation_id: String,
    },
    Status {
        operation_id: String,
    },
    Abort {
        operation_id: String,
    },
}
impl TransferRequest {
    pub fn operation_id(&self) -> &str {
        match self {
            Self::Begin { operation_id, .. }
            | Self::Advance { operation_id }
            | Self::Inventory { operation_id, .. }
            | Self::Append { operation_id, .. }
            | Self::Missing { operation_id, .. }
            | Self::Read { operation_id, .. }
            | Self::Write { operation_id, .. }
            | Self::Commit { operation_id }
            | Self::Status { operation_id }
            | Self::Abort { operation_id } => operation_id,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TransferPhase {
    Scanning,
    Inventory,
    Content,
    Staging,
    Ready,
    Complete,
    Aborted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TransferStatus {
    pub operation_id: String,
    pub phase: TransferPhase,
    pub entries: u32,
    pub bytes: u64,
    pub transferred_bytes: u64,
    pub reused_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "result",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TransferResponse {
    Status(TransferStatus),
    Inventory {
        entries: Vec<InventoryEntry>,
        next_offset: Option<u32>,
    },
    Missing {
        digests: Vec<String>,
        next_offset: Option<u32>,
    },
    Chunk {
        data: ByteChunk,
        eof: bool,
    },
    Written {
        next_offset: u64,
    },
}
