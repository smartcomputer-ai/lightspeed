//! The frozen bootstrap surface besides the `registration/challenge` frame:
//! the envd discovery document the gateway serves at
//! [`DISCOVERY_PATH`](crate::routes::DISCOVERY_PATH), and the build facts
//! `lightspeed-envd --print-build` prints.
//!
//! Daemons of every version read these to upgrade themselves, so they only
//! ever gain fields and are decoded tolerantly: unknown members are
//! ignored.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::bytes::hex_value;

hex_value!(
    /// `sha256` of a published archive: 64 lowercase hex digits.
    ArtifactSha256,
    32,
    "^[0-9a-f]{64}$"
);

/// The discovery document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryDocument {
    /// Release version of the published envd.
    pub version: String,
    pub git_sha: String,
    /// Release channel, such as `stable` or `snapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The published envd's `ENVD_PROTOCOL_VERSION`.
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_at_ms: Option<i64>,
    /// Archives by Rust target triple.
    pub artifacts: BTreeMap<String, DiscoveryArtifact>,
}

/// One published envd archive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryArtifact {
    /// Archive file name.
    pub file: String,
    /// Digest of the archive's bytes.
    pub sha256: ArtifactSha256,
    /// Absolute download URL; absent (or null) when the archive is served
    /// next to the document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What `lightspeed-envd --print-build` prints, as one JSON object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BuildFacts {
    /// Always `lightspeed-envd`.
    pub name: String,
    pub version: String,
    pub git_sha: String,
    /// Rust target triple.
    pub target: String,
    /// The binary's `ENVD_PROTOCOL_VERSION`.
    pub protocol_version: u32,
}
