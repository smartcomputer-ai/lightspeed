//! Data-plane `initialize`.
//!
//! `initialize` is the first request on every data connection; any other
//! method before it is `notInitialized`, and a second `initialize` is
//! `invalidRequest`. A `protocolVersion` other than
//! [`ENVD_PROTOCOL_VERSION`](crate::ENVD_PROTOCOL_VERSION) is
//! `versionMismatch`. The connection stays open after all three.

use serde::{Deserialize, Serialize};

use crate::{
    handshake::{ImplementationInfo, Plane},
    ids::DaemonInstanceId,
    paths::EnvironmentPath,
    serde_util::is_false,
};

/// `initialize` params on the data plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeParams {
    /// The client's `ENVD_PROTOCOL_VERSION`.
    pub protocol_version: u32,
    pub client_info: ImplementationInfo,
}

/// `initialize` response on the data plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeResponse {
    pub protocol_version: u32,
    /// Always `data`.
    pub plane: Plane,
    /// Random per daemon start; a different value than on an earlier
    /// connection means the daemon restarted and the earlier instance's
    /// process handles are gone.
    pub daemon_instance_id: DaemonInstanceId,
    pub server_info: ImplementationInfo,
    pub capabilities: DataCapabilities,
    pub limits: DataLimits,
    /// The daemon's working directory: the default `cwd` of commands.
    pub default_cwd: EnvironmentPath,
    /// Home directory of the user commands run as, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_directory: Option<EnvironmentPath>,
}

/// Capability families. A present family means its base methods exist; a
/// family's fields are facts that vary per instance. A request to an
/// absent family, or one needing an absent fact, is
/// `capabilityUnavailable`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataCapabilities {
    /// `fs/*` (search, glob and scan included) and `transfer/*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemCapabilities>,
    /// `process/*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessCapabilities>,
    /// `job/*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobCapabilities>,
}

/// The `filesystem` family: `fs/*` and `transfer/*`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemCapabilities {
    /// Methods that change the filesystem are served; false on a daemon
    /// started read-only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub write: bool,
}

/// The `process` family: `process/*`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessCapabilities {
    /// `process/start` accepts `pty`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pty: bool,
}

/// The `job` family: `job/*`. It has no instance facts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobCapabilities {}

/// Operational ceilings this daemon enforces. A request above a ceiling is
/// `invalidParams` naming the field and the ceiling; nothing is clamped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataLimits {
    /// Requests in flight per connection before `busy`; at least
    /// [`MIN_CONCURRENT_REQUESTS`](crate::limits::MIN_CONCURRENT_REQUESTS).
    pub max_concurrent_requests: u32,
    /// Largest `waitMs` of `process/read` and `job/read`.
    pub max_wait_ms: u64,
    pub process: ProcessLimits,
    pub job: JobLimits,
    pub filesystem: FilesystemLimits,
}

/// Ceilings of `process/*`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessLimits {
    /// Largest `maxBytes` of one `process/read`.
    pub max_read_bytes: u64,
    /// Output a process retains; beyond it the oldest delivered bytes are
    /// released (`expired` gaps), then the undelivered middle (`overflow`
    /// gaps).
    pub max_retained_output_bytes: u64,
    /// How long an exited process stays readable after a read first
    /// returned its settled exit with nothing left unread.
    pub retention_after_exit_ms: u64,
    /// How long an exited process whose output was never fully read stays
    /// readable.
    pub unread_retention_ms: u64,
    /// Output retained across all exited processes; beyond it the oldest
    /// are evicted early.
    pub max_exited_retained_bytes: u64,
    /// Grace between `SIGTERM` and `SIGKILL` for `terminate` and timeouts.
    pub terminate_grace_ms: u64,
    /// Bytes queued for a process's input before `process/write` accepts
    /// fewer than it was sent.
    pub max_input_queue_bytes: u64,
}

/// Ceilings of `job/*`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobLimits {
    /// Largest `maxBytes` of one `job/read`, across its entries.
    pub max_read_bytes: u64,
    /// Most jobs one `job/start`, `job/read` or `job/cancel` names, and most
    /// entries a `job/cancel` response lists.
    pub max_request_jobs: u32,
    /// Output a job retains: the head and the tail, each half of this; the
    /// middle is an `overflow` gap.
    pub max_retained_output_bytes: u64,
    /// How long a terminal job's record and output are kept.
    pub record_retention_ms: u64,
    /// Grace between `SIGTERM` and `SIGKILL` for a non-forced cancel and
    /// timeouts.
    pub cancel_grace_ms: u64,
}

/// Ceilings of `fs/*` and `transfer/*`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilesystemLimits {
    /// Largest `maxBytes` of `fs/readFile`.
    pub max_read_file_bytes: u64,
    /// Largest `data` of `fs/writeFile`.
    pub max_write_file_bytes: u64,
    /// Largest `limit` of `fs/readDirectory`.
    pub max_directory_entries: u32,
    pub search: SearchCeilings,
    pub glob: GlobCeilings,
    pub scan: ScanCeilings,
    pub transfer: TransferCeilings,
}

/// Ceilings of `fs/searchText` quotas, field for field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchCeilings {
    pub max_matches: u32,
    pub max_files: u32,
    pub max_bytes: u64,
    pub max_duration_ms: u64,
    pub max_line_bytes: u32,
}

/// Ceilings of `fs/globFiles` quotas, field for field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobCeilings {
    pub max_matches: u32,
    pub max_files: u32,
    pub max_duration_ms: u64,
}

/// Ceilings of `fs/scan`: root and pattern counts, then its quotas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanCeilings {
    pub max_roots: u32,
    pub max_patterns: u32,
    pub max_entries: u32,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_duration_ms: u64,
}

/// Ceilings of `transfer/*`: capacity, page and chunk sizes, receipt
/// retention, then the per-operation quotas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferCeilings {
    /// Operations not yet complete, aborted or failed, across connections;
    /// a begin beyond it is `busy` (`transferCapacity`).
    pub max_active_operations: u32,
    pub max_content_chunk_bytes: u64,
    pub max_inventory_page_entries: u32,
    /// How long a finished operation's receipt is answered.
    pub receipt_retention_ms: u64,
    pub max_entries: u32,
    pub max_depth: u32,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_duration_ms: u64,
}
