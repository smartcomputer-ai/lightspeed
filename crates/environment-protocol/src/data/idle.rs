//! `env/getIdle`: the daemon's activity report for idle power policy.
//!
//! The daemon reports a monotonic idle duration, never a wall-clock
//! timestamp: after a freeze, stateful stop or snapshot restore the guest
//! clock is stale until it resyncs, while a monotonic duration stays
//! correct. The policy is the provider's
//! ([`IdlePolicy`](crate::idle::IdlePolicy)); it decides on `idleForMs`
//! alone, and the counts are diagnostics.

use serde::{Deserialize, Serialize};

use crate::serde_util::is_zero_u32;

/// `env/getIdle` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetIdleParams {}

/// `env/getIdle` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetIdleResponse {
    /// Milliseconds since the last `use` request, or since running work
    /// last ended, whichever is later. Zero while any process or job runs
    /// or any leftover process group has live members; the clock restarts
    /// when the last of them ends.
    pub idle_for_ms: u64,
    /// Processes started with `process/start` whose root has not exited.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub running_process_count: u32,
    /// Jobs that are `running`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub running_job_count: u32,
    /// Process groups of exited processes that still have members alive:
    /// services a command started and left running. They hold `idleForMs`
    /// at zero like running work.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub leftover_process_group_count: u32,
}
