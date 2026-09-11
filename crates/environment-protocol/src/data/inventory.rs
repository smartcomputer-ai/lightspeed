//! Portable filesystem inventories. Digests identify raw file bytes, not paths or modes.
use crate::shared::{ByteChunk, EnvironmentPath};
use serde::{Deserialize, Serialize};

pub const MAX_INVENTORY_PAGE: usize = 128;
pub const MAX_CONTENT_CHUNK: usize = 256 * 1024;
pub const MAX_INVENTORY_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InventoryContent {
    Directory,
    File {
        size_bytes: u64,
        executable: bool,
        digest: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryEntry {
    /// Empty for the selected root; otherwise a strict relative slash-separated path.
    pub path: String,
    pub content: InventoryContent,
}

/// Total operation quotas, separate from wire chunk and page limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryLimits {
    pub max_entries: u32,
    pub max_depth: u32,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_duration_ms: u64,
}
impl Default for InventoryLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_depth: 64,
            max_file_bytes: 1024_u64.pow(4),
            max_total_bytes: 1024_u64.pow(4),
            max_manifest_bytes: 32 * 1024 * 1024,
            max_duration_ms: 24 * 60 * 60 * 1000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanDigestAlgorithm {
    Sha256,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScanContent {
    Directory,
    File {
        size_bytes: u64,
        executable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
}

/// Small, bounded observation for catalogs. Large inventories use transfer sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanParams {
    pub roots: Vec<EnvironmentPath>,
    /// Filename-only patterns without `/` or `**` select direct children only.
    /// Other patterns permit recursive traversal within the operation quotas.
    #[serde(default)]
    pub include_patterns: Vec<String>,
    #[serde(default)]
    pub read_content: bool,
    /// Follow aliases only inside the endpoint filesystem access scope.
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub digest_algorithm: Option<ScanDigestAlgorithm>,
    #[serde(default)]
    pub limits: InventoryLimits,
    pub if_none_match: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    /// Resolved absolute identity, while root/path retain the requested spelling.
    pub canonical_path: EnvironmentPath,
    pub root: EnvironmentPath,
    pub path: String,
    pub content: ScanContent,
    pub data: Option<ByteChunk>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanDiagnostic {
    pub root: EnvironmentPath,
    pub error: crate::error::EnvironmentProtocolError,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResponse {
    pub fingerprint: Option<String>,
    pub unchanged: bool,
    pub complete: bool,
    pub entries: Vec<ScanEntry>,
    pub diagnostics: Vec<ScanDiagnostic>,
}
