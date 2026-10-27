//! `process/*`: interactive processes read through byte-offset pages.
//!
//! A process runs as the leader of its own process group. Its root's
//! natural exit never sweeps the group: whatever it left running is
//! reported in `leftovers` and keeps the environment's lifetime. Each
//! process has a daemon-owned cursor; a read without `after` starts there,
//! and the cursor advances to `next` only after the response frame was
//! written to the connection.

use serde::{Deserialize, Serialize};

use crate::{
    bytes::ByteChunk,
    data::exec::{
        CommandSpec, ExecState, ExitStatus, LeftoverReport, OutputPage, PtySize, ReturnOn,
        StoppedBy,
    },
    error::ProtocolError,
    ids::ProcessId,
    serde_util::is_false,
};

/// `process/start` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartProcessParams {
    /// Caller-chosen handle, reserved before any side effect: a start whose
    /// id is already in use (running, retained or remembered by a tombstone)
    /// is `alreadyExists` and runs nothing.
    pub process_id: ProcessId,
    pub command: CommandSpec,
    /// Run under a PTY of this size. The PTY master is the input: `input`
    /// must then be `none` or `bytes`, and `process/write` is always
    /// accepted while the process runs. Requires
    /// `capabilities.process.pty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty: Option<PtySize>,
}

/// `process/start` response, sent once the process runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartProcessResponse {
    /// OS pid of the root process, which is also its process group id.
    pub pid: u32,
}

/// `process/read` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadProcessParams {
    pub process_id: ProcessId,
    /// Start offset. Absent: the daemon cursor, which this read advances to
    /// `next` once its response is written. Present: a non-consuming read
    /// that leaves the cursor alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// Start at `end - tailBytes` instead (clamped at 0). Non-consuming;
    /// excludes `after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_bytes: Option<u64>,
    /// Acknowledge every byte before this offset first: the cursor moves to
    /// it (never backwards) whether or not this read's response arrives. A
    /// caller that tracks positions reads with `after` and acknowledges
    /// with `ack` for exactly-once delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<u64>,
    /// Most `data` bytes to return, at most `limits.process.maxReadBytes`.
    /// 0 reads only the state.
    pub max_bytes: u64,
    /// How long to wait, at most `limits.maxWaitMs`. 0 polls.
    pub wait_ms: u64,
    pub return_on: ReturnOn,
}

/// `process/read` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadProcessResponse {
    pub output: OutputPage,
    pub state: ExecState,
    /// How the root ended; present once `state` is `exited`, unless `error`
    /// explains why it could not be observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitStatus>,
    /// Set when the daemon stopped the process; absent on a natural exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<StoppedBy>,
    /// Infrastructure failure after start (output capture or wait failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
    /// OS pid of the root process.
    pub pid: u32,
    /// Group members alive after the root exited (`disposition: kept`),
    /// sampled when this response was built. Absent while running, when
    /// none remain, and on platforms without process enumeration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leftovers: Option<LeftoverReport>,
}

/// `process/write` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteProcessParams {
    pub process_id: ProcessId,
    /// Bytes for the process's input pipe or PTY.
    #[serde(default, skip_serializing_if = "ByteChunk::is_empty")]
    pub data: ByteChunk,
    /// Close the input after `data`: end of file on a pipe, `VEOF` on a
    /// PTY. Applied only when every byte of `data` was accepted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub close_input: bool,
}

/// `process/write` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteProcessResponse {
    /// Leading bytes of `data` queued to the process's writer. Fewer than
    /// sent (possibly 0) means the input queue is full: resend the rest
    /// later.
    pub accepted_bytes: u64,
}

/// Signal delivered by `process/terminate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ProcessSignal {
    /// `SIGINT` to the foreground process group (the terminal's under a
    /// PTY, otherwise the process group). Records nothing; the next read
    /// shows what the process did.
    Interrupt,
    /// `SIGTERM` to the group, then `SIGKILL` after
    /// `limits.process.terminateGraceMs` if members remain. Records
    /// `stoppedBy: terminate`.
    Terminate,
    /// `SIGKILL` to the group. Records `stoppedBy: kill`.
    Kill,
}

/// `process/terminate` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminateProcessParams {
    pub process_id: ProcessId,
    pub signal: ProcessSignal,
}

/// `process/terminate` response, sent once the signal was delivered (any
/// escalation continues in the background).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminateProcessResponse {
    /// True when the root process was still running when the signal was
    /// sent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub running: bool,
}
