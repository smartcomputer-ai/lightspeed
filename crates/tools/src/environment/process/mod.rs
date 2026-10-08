//! Process execution capability boundary.
//!
//! The substrate is two operations that are the same for every provider
//! surface: `run_process` starts a command and waits up to a yield or to
//! exit; `continue_process` takes the handle of a still-running command,
//! optionally delivers input, closes stdin, or sends a signal, then waits for
//! its window and returns the output produced since the last call. The
//! executor owns nothing across calls: the read cursor lives with the
//! process, so a handle works from any later call or connection.

use std::collections::BTreeMap;

use async_trait::async_trait;
use environment_protocol::shared::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fs::FsPath;

pub mod local;

pub type ProcessExecResult<T> = Result<T, ProcessError>;

#[async_trait]
pub trait ProcessExecutor: Send + Sync {
    async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput>;

    async fn continue_process(
        &self,
        request: ContinueProcessRequest,
    ) -> ProcessExecResult<ProcessOutput>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRequest {
    pub argv: Vec<String>,
    pub cwd: Option<FsPath>,
    pub env: BTreeMap<String, String>,
    pub secret_env: BTreeMap<String, SecretString>,
    /// One-shot standard input, written and closed at start.
    pub stdin: Option<Vec<u8>>,
    /// Allocate a PTY. Interactive input through `continue_process` needs
    /// one; plain pipes read end of file from `/dev/null`.
    pub tty: bool,
    /// Kill deadline. Absent means the call never kills a running process.
    pub timeout_ms: Option<u64>,
    /// Return with a handle after this long if the process is still
    /// running. Absent waits until exit.
    pub yield_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

impl ProcessRequest {
    pub fn argv<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            cwd: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            stdin: None,
            tty: false,
            timeout_ms: None,
            yield_ms: None,
            max_output_bytes: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinueProcessRequest {
    pub handle: ProcessHandle,
    /// Bytes for the process's input; requires a PTY.
    pub input: Option<Vec<u8>>,
    pub close_stdin: bool,
    pub signal: Option<ProcessSignal>,
    /// Collect output for this long, returning early only when the process
    /// exits. Absent waits until exit.
    pub wait_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

impl ContinueProcessRequest {
    pub fn wait(handle: ProcessHandle, wait_ms: Option<u64>) -> Self {
        Self {
            handle,
            input: None,
            close_stdin: false,
            signal: None,
            wait_ms,
            max_output_bytes: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSignal {
    /// `SIGINT` to the process group; the following read observes whatever
    /// the process did with it.
    Interrupt,
    /// Kill the process group.
    Kill,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ProcessHandle(pub String);

impl ProcessHandle {
    pub fn new(handle: impl Into<String>) -> Self {
        Self(handle.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProcessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProcessOutput {
    pub status: ProcessStatus,
    /// Present while the process is still running.
    pub handle: Option<ProcessHandle>,
    /// OS pid of the root process, which is also its process group id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub stdout: StreamOutput,
    pub stderr: StreamOutput,
    /// Bytes dropped from the middle of the output because more was
    /// produced than the environment retains between reads.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_bytes: u64,
    /// Processes still running in the command's group after it exited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leftover_processes: Vec<LeftoverProcess>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LeftoverProcess {
    pub pid: u32,
    pub command: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Killed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StreamOutput {
    pub bytes: Vec<u8>,
    /// Byte offset in `bytes` before which `ProcessOutput::omitted_bytes`
    /// were dropped, when the omission fell in this stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_at: Option<usize>,
}

impl StreamOutput {
    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("process execution unsupported: {message}")]
    Unsupported { message: String },

    #[error("invalid process request: {message}")]
    InvalidRequest { message: String },

    #[error(
        "unknown process handle {handle}: the process finished and its output retention elapsed, or it was never started here"
    )]
    UnknownHandle { handle: ProcessHandle },

    #[error(
        "stdin is closed for process {handle}: it was started without a PTY or has exited; start the command with tty: true to send it input"
    )]
    StdinClosed { handle: ProcessHandle },

    #[error("process execution failed: {message}")]
    Failed { message: String },
}
