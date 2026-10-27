//! `transfer/*`: one logical tree or file transfer as many bounded calls.
//!
//! No storage or VFS identities cross this boundary: entries are tree paths
//! and content digests. An operation is created by `transfer/begin` and
//! keyed by its caller-chosen `operationId`; retrying any call with the same
//! input is safe. Phases:
//!
//! - **capture** (environment to caller): `scanning` (advance until the
//!   inventory is sealed) → `inventory` (page entries, read content) →
//!   `complete` after `transfer/commit`.
//! - **materialize** and **writeFile** (caller to environment):
//!   `inventory` (append the incoming inventory) → `scanning` (the daemon
//!   reads destination metadata and hashes only files whose size matches an
//!   incoming entry) → `content` (upload `transfer/listMissing` digests) →
//!   `staging` (advance) → `ready` → `complete` after `transfer/commit`,
//!   which publishes the staged tree atomically.
//!
//! A failure of the operation's own work (walking, hashing, staging,
//! publishing, a source changing, a digest mismatch, its duration bound)
//! moves it to `failed` with its `error`, and every later call except
//! `transfer/getStatus` and `transfer/abort` answers that error; nothing is
//! committed. A call refused before it acts (invalid params, a phase or
//! offset precondition, an unknown digest, `busy`) leaves the operation
//! unchanged, so a retried or misdirected call cannot fail it. A completed, aborted or failed operation's
//! receipt is answered for `limits.filesystem.transfer.receiptRetentionMs`
//! after it finished, across daemon restarts. A completed capture's
//! inventory stays pageable with `transfer/readInventory` for as long as its
//! receipt, so a retried capture answers the tree it first captured; its
//! content is no longer readable.

use serde::{Deserialize, Serialize};

use crate::{
    bytes::ByteChunk,
    data::fs::{FileVersion, TreeContent, TreeFilter, WriteCondition},
    error::ProtocolError,
    ids::TransferOperationId,
    paths::{ContentDigest, EnvironmentPath, TreePath},
    serde_util::{is_false, is_zero_u32},
};

/// What one operation transfers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TransferSelection {
    /// Read a file or tree out of the environment.
    Capture {
        source: EnvironmentPath,
        #[serde(default, skip_serializing_if = "TreeFilter::is_default")]
        filter: TreeFilter,
        #[serde(default, skip_serializing_if = "SymlinkPolicy::is_default")]
        symlinks: SymlinkPolicy,
        #[serde(default, skip_serializing_if = "SpecialFilePolicy::is_default")]
        special_files: SpecialFilePolicy,
    },
    /// Write a tree into the environment, replacing `destination` atomically
    /// at commit.
    Materialize {
        destination: EnvironmentPath,
        #[serde(default, skip_serializing_if = "TransferOnExisting::is_default")]
        on_existing: TransferOnExisting,
    },
    /// Write exactly one file with `fs/writeFile` semantics: staged,
    /// atomic, an existing target keeps its permissions, an in-root symlink
    /// at `destination` is followed.
    WriteFile {
        destination: EnvironmentPath,
        #[serde(default, skip_serializing_if = "WriteCondition::is_default")]
        create: WriteCondition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_match: Option<FileVersion>,
        #[serde(default, skip_serializing_if = "is_false")]
        executable: bool,
    },
}

/// Symlinks met during a capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SymlinkPolicy {
    /// Capture a link whose target stays inside the selection as the entry
    /// it points to; a link leaving the selection or a cycle fails the
    /// operation (`symlinkRejected`). A dangling link inside the selection
    /// is left out and reported in `skipped` as `symlink`.
    #[default]
    ExpandInside,
    /// Leave every link out and report it in `skipped`.
    Skip,
    /// Fail on the first link.
    Reject,
}

impl SymlinkPolicy {
    pub fn is_default(&self) -> bool {
        *self == Self::ExpandInside
    }
}

/// Devices, FIFOs and sockets met during a capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SpecialFilePolicy {
    /// Fail the operation (`notRegularFile`).
    #[default]
    Reject,
    /// Leave them out and report them in `skipped`.
    Skip,
}

impl SpecialFilePolicy {
    pub fn is_default(&self) -> bool {
        *self == Self::Reject
    }
}

/// What a materialize does with an existing destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TransferOnExisting {
    /// Refuse at begin (`alreadyExists`).
    Error,
    /// Replace it atomically at commit.
    #[default]
    Replace,
}

impl TransferOnExisting {
    pub fn is_default(&self) -> bool {
        *self == Self::Replace
    }
}

/// Caller quotas of one operation; each at most its advertised ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferLimits {
    /// Entries, the selected root included.
    pub max_entries: u32,
    /// The selected root has depth 0.
    pub max_depth: u32,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    /// Encoded inventory size.
    pub max_manifest_bytes: u64,
    /// From begin to commit.
    pub max_duration_ms: u64,
}

/// One inventory entry. Parents precede children.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryEntry {
    /// Relative to the selected root; `.` is the root itself.
    pub path: TreePath,
    /// A file entry always carries its `digest`.
    pub content: TreeContent,
}

/// `transfer/begin` params. Idempotent: a repeat with identical input
/// returns the operation's status; different input under the same id is
/// `failedPrecondition` (`operationIdInUse`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeginTransferParams {
    pub operation_id: TransferOperationId,
    pub selection: TransferSelection,
    pub limits: TransferLimits,
}

/// Params of `transfer/advance`, `transfer/getStatus`, `transfer/commit`
/// and `transfer/abort`: each names only the operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferOperationParams {
    pub operation_id: TransferOperationId,
}

/// The operation's status: the response of `transfer/begin`,
/// `transfer/advance`, `transfer/getStatus`, `transfer/append`,
/// `transfer/commit` and `transfer/abort`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferStatus {
    pub operation_id: TransferOperationId,
    pub phase: TransferPhase,
    /// Entries in the inventory so far.
    pub entry_count: u32,
    /// File bytes in the inventory so far.
    pub total_bytes: u64,
    /// Content bytes moved over the wire.
    pub transferred_bytes: u64,
    /// Content bytes satisfied from files already at the destination.
    pub reused_bytes: u64,
    /// The first
    /// [`MAX_LISTED_SKIPS`](crate::limits::MAX_LISTED_SKIPS) entries a
    /// capture left out under its policies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<TransferSkip>,
    /// Total number of entries left out, listed or not; present whenever it
    /// is not zero.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub skipped_entry_count: u32,
    /// Why the operation failed; present exactly when `phase` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
    /// When `maxDurationMs` runs out for an active operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,
    /// When the receipt of a finished (complete, aborted or failed)
    /// operation stops being answered; afterwards calls are `notFound`
    /// (`expired`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_expires_at_ms: Option<i64>,
}

/// Where an operation stands; see the module docs for the order per
/// selection kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TransferPhase {
    /// The daemon walks a tree: the capture source, or the destination of a
    /// materialize. Advance until the phase changes.
    Scanning,
    /// Capture: the inventory is sealed and pageable. Materialize: waiting
    /// for appended entries.
    Inventory,
    /// Waiting for the content of `transfer/listMissing` digests.
    Content,
    /// The daemon assembles the staged tree. Advance until `ready`.
    Staging,
    /// Ready for `transfer/commit`.
    Ready,
    Complete,
    Aborted,
    /// Stopped by an error; see `error`.
    Failed,
}

impl TransferPhase {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Complete | Self::Aborted | Self::Failed)
    }
}

/// An entry a capture left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferSkip {
    /// Absent when the entry's name is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<TreePath>,
    pub reason: TransferSkipReason,
}

/// Why a capture left an entry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TransferSkipReason {
    /// `symlinks: skip`, or a dangling link under `symlinks: expandInside`.
    Symlink,
    /// `specialFiles: skip`.
    SpecialFile,
    /// The name is not UTF-8; such entries are always skipped.
    NonUtf8Name,
}

/// `transfer/readInventory` params: page a sealed inventory by index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadTransferInventoryParams {
    pub operation_id: TransferOperationId,
    /// Index of the first entry.
    pub index: u32,
    /// 1 to `limits.filesystem.transfer.maxInventoryPageEntries`.
    pub limit: u32,
}

/// `transfer/readInventory` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadTransferInventoryResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<InventoryEntry>,
    /// Index of the next page; absent after the last entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_index: Option<u32>,
}

/// `transfer/append` params: add incoming inventory entries in order.
/// Retrying an identical page is safe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppendTransferInventoryParams {
    pub operation_id: TransferOperationId,
    /// Index of the page's first entry: the number of entries appended
    /// before it.
    pub index: u32,
    /// At most `limits.filesystem.transfer.maxInventoryPageEntries`.
    pub entries: Vec<InventoryEntry>,
    /// This page completes the inventory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub last: bool,
}

/// `transfer/listMissing` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListTransferMissingParams {
    pub operation_id: TransferOperationId,
    /// Most digests to return, 1 to
    /// `limits.filesystem.transfer.maxInventoryPageEntries`.
    pub limit: u32,
}

/// `transfer/listMissing` response: digests whose content the daemon still
/// needs. The set shrinks as uploads complete; call again rather than
/// paging.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListTransferMissingResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub digests: Vec<ContentDigest>,
    /// Missing digests beyond this page.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub remaining_count: u32,
}

/// `transfer/read` params: read a captured file's content by digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadTransferContentParams {
    pub operation_id: TransferOperationId,
    pub digest: ContentDigest,
    pub offset: u64,
    /// 1 to `limits.filesystem.transfer.maxContentChunkBytes`.
    pub max_bytes: u64,
}

/// `transfer/read` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadTransferContentResponse {
    pub data: ByteChunk,
    /// Bytes exist after `offset` plus the length of `data`, as for
    /// `fs/readFile`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// `transfer/write` params: upload content for a missing digest. Chunks for
/// one digest arrive in order; an identical chunk may be retried.
/// Different digests may be uploaded concurrently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteTransferContentParams {
    pub operation_id: TransferOperationId,
    pub digest: ContentDigest,
    /// Must equal the digest's next expected offset.
    pub offset: u64,
    /// At most `limits.filesystem.transfer.maxContentChunkBytes`.
    pub data: ByteChunk,
}

/// `transfer/write` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteTransferContentResponse {
    /// The digest's next expected offset; equal to its size once complete
    /// and verified.
    pub next_offset: u64,
}
