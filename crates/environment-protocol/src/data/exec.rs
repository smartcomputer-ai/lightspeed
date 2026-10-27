//! The execution primitive processes and jobs share: the command, its
//! input, its output stream and its outcome.
//!
//! **Output positions.** A handle's output is one byte stream: stdout,
//! stderr and the PTY interleaved in arrival order. Positions are byte
//! offsets into it. `end` is the number of bytes produced so far; a page
//! cut at `maxBytes` loses nothing because the next read starts at `next`;
//! a tail read starts at `end - tailBytes`. Bytes the daemon no longer
//! retains are reported as typed [`OutputGap`]s, never skipped silently.
//!
//! **Settled exit.** `state` becomes `exited` only once the root process
//! has exited and its output has settled (every reader reached end of file
//! or was discarded), so `end` is final from then on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    bytes::{ByteChunk, SecretString},
    paths::EnvironmentPath,
    serde_util::{is_false, is_zero_u32},
};

/// What to run. Shared by `process/start` and `job/start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandSpec {
    /// Program and arguments; `argv[0]` is resolved through `PATH`. Not
    /// empty.
    pub argv: Vec<String>,
    /// Working directory; the daemon's `defaultCwd` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<EnvironmentPath>,
    /// Variables added to the daemon's scrubbed environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Like `env`, but every value is redacted from the command's output.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, SecretString>,
    /// Standard input; none by default.
    #[serde(default, skip_serializing_if = "ExecInput::is_none")]
    pub input: ExecInput,
    /// Stop deadline from start. On expiry the daemon terminates the process
    /// group (`SIGTERM`, the advertised grace, then `SIGKILL`) and records
    /// `stoppedBy: timeout`. Absent means no deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Standard input of a command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExecInput {
    /// `/dev/null`: the command reads end of file. Model-facing tools use
    /// this.
    None {},
    /// These bytes, then end of file. Written by a writer task after the
    /// output readers, the exit watcher and the deadline are in place.
    Bytes { data: ByteChunk },
    /// A persistent pipe fed by `process/write` until closed. Processes
    /// only; `job/start` refuses it with `invalidParams`.
    Pipe {},
}

impl Default for ExecInput {
    fn default() -> Self {
        Self::None {}
    }
}

impl ExecInput {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None {})
    }
}

/// Terminal size of a PTY. Each dimension is 1 to 4096.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PtySize {
    pub rows: u32,
    pub cols: u32,
}

impl PtySize {
    pub const MAX_DIMENSION: u32 = 4096;
}

/// Which descriptor produced a chunk. Jobs never produce `pty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum OutputStream {
    Stdout,
    Stderr,
    /// The PTY master, which merges stdout and stderr.
    Pty,
}

/// Contiguous output bytes from one stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputChunk {
    /// Byte offset of the chunk's first byte in the combined output.
    pub offset: u64,
    pub stream: OutputStream,
    pub data: ByteChunk,
}

/// Output bytes in `[offset, offset + omittedBytes)` that a page cannot
/// return.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputGap {
    pub offset: u64,
    pub omitted_bytes: u64,
    pub reason: GapReason,
}

/// Why a range of output cannot be returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum GapReason {
    /// Never delivered: dropped from the middle because retained output
    /// exceeded the handle's cap. The head and the newest bytes are kept.
    Overflow,
    /// Already delivered through the daemon cursor (or acknowledged) and
    /// released since.
    Expired,
}

/// One page of a handle's output. Returned by `process/read` and in every
/// `job/read` entry. A page holds at most
/// [`MAX_PAGE_CHUNKS`](crate::limits::MAX_PAGE_CHUNKS) chunks and gaps
/// together (for `job/read`, across all entries); one that reaches it ends
/// early, and `next` says where to continue.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputPage {
    /// In offset order, at most `maxBytes` of `data` in total.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<OutputChunk>,
    /// Offset to read from next: past the last returned byte or gap.
    pub next: u64,
    /// Bytes produced so far; final once the handle has exited.
    pub end: u64,
    /// Unreturnable ranges between the requested start and `next`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<OutputGap>,
}

/// When a waiting read returns. Every mode also returns when `waitMs`
/// elapses, when the handle reaches a settled exit, or at once when there
/// is nothing left to wait for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ReturnOn {
    /// Collect for the whole window; return early only when unread output
    /// reaches `maxBytes`.
    Window,
    /// Return as soon as any output past the start position exists.
    AnyOutput,
    /// Return only at the settled exit (for `job/read`: once every entry is
    /// terminal), even if more than `maxBytes` of output accumulates.
    Exit,
}

/// Lifecycle state of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ExecState {
    Running,
    /// The root exited and its output settled.
    Exited,
}

/// How the root process ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExitStatus {
    /// Normal exit with this status code.
    Code { code: i32 },
    /// Killed by this signal.
    Signal {
        signal: SignalName,
        #[serde(default, skip_serializing_if = "is_false")]
        core_dumped: bool,
    },
}

/// Why the daemon stopped a process or job that did not exit on its own.
/// Set only when the daemon signalled a root that was still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum StoppedBy {
    /// `timeoutMs` expired: `SIGTERM`, the grace, then `SIGKILL`.
    Timeout,
    /// `process/terminate` with `terminate`, or `job/cancel` without
    /// `force`: `SIGTERM`, the grace, then `SIGKILL`.
    Terminate,
    /// `process/terminate` with `kill`, or `job/cancel` with `force`:
    /// `SIGKILL` at once.
    Kill,
}

/// A POSIX signal name such as `SIGSEGV` or `SIGRTMIN+3`: `SIG` followed by
/// 1 to 16 uppercase letters, digits or `+`. Names, not numbers, because
/// numbers differ between platforms.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SignalName(
    #[cfg_attr(feature = "schema", schemars(pattern(SIGNAL_NAME_PATTERN)))] String,
);

/// JSON Schema pattern of [`SignalName`].
pub const SIGNAL_NAME_PATTERN: &str = "^SIG[A-Z0-9+]{1,16}$";

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("signal name must be SIG followed by 1..=16 uppercase letters, digits or '+': {0:?}")]
pub struct SignalNameError(pub String);

impl SignalName {
    pub fn new(name: impl Into<String>) -> Result<Self, SignalNameError> {
        let name = name.into();
        let valid = name.strip_prefix("SIG").is_some_and(|rest| {
            (1..=16).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'+')
        });
        if valid {
            Ok(Self(name))
        } else {
            Err(SignalNameError(name))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SignalName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for SignalName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SignalName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::new(name).map_err(serde::de::Error::custom)
    }
}

/// Processes of a command's group still alive after its root exited.
/// Present only when there is at least one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeftoverReport {
    pub disposition: LeftoverDisposition,
    /// At most [`MAX_LEFTOVER_PROCESSES`](crate::limits::MAX_LEFTOVER_PROCESSES),
    /// lowest pid first; never empty.
    pub processes: Vec<LeftoverProcess>,
    /// Leftovers beyond the listed ones.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub omitted_process_count: u32,
}

/// What the daemon did with leftovers. The policies differ on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LeftoverDisposition {
    /// Left running (processes): they keep the environment's lifetime and
    /// are reported until they exit.
    Kept,
    /// Terminated when the job finished (jobs); the submitter should fix
    /// its script's teardown.
    Swept,
}

/// One leftover process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeftoverProcess {
    pub pid: u32,
    /// Command line when the platform exposes it, otherwise the executable
    /// name, cut to
    /// [`MAX_LEFTOVER_COMMAND_BYTES`](crate::limits::MAX_LEFTOVER_COMMAND_BYTES)
    /// on a character boundary; absent when neither could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn exit_status_names_signals() {
        let status = ExitStatus::Signal {
            signal: SignalName::new("SIGSEGV").unwrap(),
            core_dumped: true,
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({"kind": "signal", "signal": "SIGSEGV", "coreDumped": true})
        );
        assert!(SignalName::new("SIGRTMIN+3").is_ok());
        for bad in ["SEGV", "SIG", "sigsegv", "SIGSEGV!"] {
            assert!(SignalName::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn input_defaults_to_none_and_is_omitted() {
        let spec: CommandSpec = serde_json::from_value(json!({"argv": ["true"]})).unwrap();
        assert!(spec.input.is_none());
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            json!({"argv": ["true"]})
        );
        let piped: CommandSpec =
            serde_json::from_value(json!({"argv": ["cat"], "input": {"kind": "pipe"}})).unwrap();
        assert_eq!(piped.input, ExecInput::Pipe {});
        assert!(serde_json::from_value::<CommandSpec>(json!({"argv": [], "stdin": "x"})).is_err());
    }

    #[test]
    fn fieldless_input_kinds_reject_unknown_members() {
        for kind in ["none", "pipe"] {
            assert!(serde_json::from_value::<ExecInput>(json!({"kind": kind})).is_ok());
            assert!(
                serde_json::from_value::<ExecInput>(json!({"kind": kind, "data": "aGk="})).is_err(),
                "{kind}"
            );
        }
    }
}
