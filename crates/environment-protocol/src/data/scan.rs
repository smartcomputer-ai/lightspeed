//! `fs/scan`: a small, bounded observation of several roots, for catalogs
//! such as skill and prompt discovery. An observation, not use: it never
//! resets the idle clock. Large trees go through `transfer/*`.
//!
//! Each root is reported on its own, so one bad entry or one failed root
//! never hides the others.

use serde::{Deserialize, Serialize};

use crate::{
    bytes::ByteChunk,
    data::fs::{TreeContent, TreeFilter},
    error::ProtocolError,
    ids::validate_opaque_id,
    paths::{EnvironmentPath, TreePath},
    serde_util::{is_false, is_zero_u32},
};

/// `fs/scan` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanParams {
    /// 1 to `limits.filesystem.scan.maxRoots` roots.
    pub roots: Vec<ScanRoot>,
    /// Applied below every root; at most `limits.filesystem.scan.maxPatterns`
    /// patterns in total.
    #[serde(default, skip_serializing_if = "TreeFilter::is_default")]
    pub filter: TreeFilter,
    /// Return the bytes of every selected file in `data`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_content: bool,
    /// Return each selected file's `digest`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_digests: bool,
    /// Follow symlinks whose targets stay inside the filesystem root; others
    /// are skipped (`symlinkOutsideRoot`). Without it symlinks are skipped
    /// (`symlink`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_symlinks: bool,
    /// Caller quotas; each at most its ceiling.
    pub limits: ScanLimits,
}

/// One root of a scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanRoot {
    pub path: EnvironmentPath,
    /// The root's previous `fingerprint`: when the selection is unchanged
    /// the result is `unchanged` and carries no entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<ScanFingerprint>,
    /// Overrides `filter.maxDepth` for this root. `0` examines only the
    /// root itself: a `complete` result without entries proves it is an
    /// existing directory, which is how discovery checks a working
    /// directory in the same call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
}

/// Caller quotas of one scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanLimits {
    /// Selected entries per root; more fail that root (`payloadTooLarge`
    /// with `limitEntries`).
    pub max_entries: u32,
    /// Larger selected files are skipped (`fileTooLarge`).
    pub max_file_bytes: u64,
    /// Content bytes returned across all roots, in root order: a root whose
    /// content would exceed the rest fails (`payloadTooLarge` with
    /// `limitBytes`) and later roots are still scanned. Independently, a
    /// root whose encoded result (entries, skips and base64 content) would
    /// push the scan's results past
    /// [`MAX_RESULT_BYTES`](crate::limits::MAX_RESULT_BYTES) fails the same
    /// way.
    pub max_total_bytes: u64,
    /// Walk time per root; exhausting it fails that root
    /// (`deadlineExceeded`).
    pub max_duration_ms: u64,
}

/// `fs/scan` response: one result per requested root, in request order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanResponse {
    /// Never empty.
    pub roots: Vec<ScanRootResult>,
}

/// The outcome of one root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ScanRootResult {
    /// Every selected entry, plus what was skipped and why.
    Complete {
        path: EnvironmentPath,
        /// Resolved absolute path of the root when it differs from `path`
        /// (the root is a symlink).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canonical_path: Option<EnvironmentPath>,
        fingerprint: ScanFingerprint,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entries: Vec<ScanEntry>,
        /// The first
        /// [`MAX_LISTED_SKIPS`](crate::limits::MAX_LISTED_SKIPS) skipped
        /// entries, in walk order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<ScanSkip>,
        /// Total number of skipped entries, listed or not; present whenever
        /// it is not zero.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        skipped_entry_count: u32,
    },
    /// The selection matches `ifNoneMatch`.
    Unchanged {
        path: EnvironmentPath,
        fingerprint: ScanFingerprint,
    },
    /// The root does not exist.
    Missing { path: EnvironmentPath },
    /// The root could not be scanned; no entries are returned for it.
    Failed {
        path: EnvironmentPath,
        error: ProtocolError,
    },
}

/// One selected entry below a root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanEntry {
    /// Relative to the root as requested.
    pub path: TreePath,
    pub content: TreeContent,
    /// The file's bytes when `readContent` was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ByteChunk>,
}

/// An entry a scan left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanSkip {
    /// Absent when the entry's name is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<TreePath>,
    pub reason: ScanSkipReason,
}

/// Why a scan left an entry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ScanSkipReason {
    /// Larger than `maxFileBytes`.
    FileTooLarge,
    /// A symlink, and `followSymlinks` is not set.
    Symlink,
    /// A symlink whose target leaves the filesystem root, or a link cycle.
    SymlinkOutsideRoot,
    /// A device, FIFO, socket or another special file.
    SpecialFile,
    /// The name is not UTF-8.
    NonUtf8Name,
    /// The entry vanished or could not be read during the walk.
    Unreadable,
}

/// Opaque fingerprint of one root's selection, prefixed `scan:v1:`, at most
/// 255 bytes without control characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScanFingerprint(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 9, max = 255), pattern(SCAN_FINGERPRINT_PATTERN))
    )]
    String,
);

/// JSON Schema pattern of [`ScanFingerprint`].
pub const SCAN_FINGERPRINT_PATTERN: &str = "^scan:v1:[^\\u0000-\\u001F\\u007F-\\u009F]+$";

impl ScanFingerprint {
    pub const PREFIX: &'static str = "scan:v1:";

    pub fn new(value: impl Into<String>) -> Result<Self, ScanFingerprintError> {
        let value = value.into();
        if !value.starts_with(Self::PREFIX)
            || value.len() == Self::PREFIX.len()
            || validate_opaque_id("ScanFingerprint", &value).is_err()
        {
            return Err(ScanFingerprintError(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("scan fingerprint must start with scan:v1: and be at most 255 bytes: {0:?}")]
pub struct ScanFingerprintError(pub String);

impl Serialize for ScanFingerprint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ScanFingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
