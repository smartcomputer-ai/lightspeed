//! Process execution capability boundary.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::host::fs::FsPath;

pub mod local;

pub type ProcessExecResult<T> = Result<T, ProcessError>;

#[async_trait]
pub trait ProcessExecutor: Send + Sync {
    async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput>;

    async fn write_stdin(
        &self,
        request: WriteProcessStdinRequest,
    ) -> ProcessExecResult<ProcessOutput>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRequest {
    pub argv: Vec<String>,
    pub cwd: Option<FsPath>,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout_ms: Option<u64>,
    pub yield_time_ms: Option<u64>,
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
            stdin: None,
            timeout_ms: None,
            yield_time_ms: None,
            max_output_bytes: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProcessStdinRequest {
    pub handle: ProcessHandle,
    pub input: Vec<u8>,
    pub close_stdin: bool,
    pub yield_time_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub status: ProcessStatus,
    pub handle: Option<ProcessHandle>,
    pub exit_code: Option<i32>,
    pub stdout: StreamOutput,
    pub stderr: StreamOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
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

    #[error("process execution failed: {message}")]
    Failed { message: String },
}
