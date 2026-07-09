//! Durable environment job capability boundary.

use std::collections::BTreeMap;

use async_trait::async_trait;
use host_protocol::{
    data::jobs::{
        CancelJobsParams, CancelJobsResponse, JobArtifact, JobCancelScope, JobDependency,
        JobDependencyPolicy, JobOutputChunk, JobStartSpec, JobStatus, JobSummary, ListJobsParams,
        ListJobsResponse, ReadJobsParams, ReadJobsResponse, StartJobsParams, StartJobsResponse,
    },
    shared::{ByteChunk, HostPath, JobId},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fs::FsPath;

pub const JOB_START_TOOL_NAME: &str = "job_start";
pub const JOB_LIST_TOOL_NAME: &str = "job_list";
pub const JOB_READ_TOOL_NAME: &str = "job_read";

pub type JobExecResult<T> = Result<T, JobError>;

#[async_trait]
pub trait JobExecutor: Send + Sync {
    async fn start_jobs(&self, request: StartJobsParams) -> JobExecResult<StartJobsResponse>;

    async fn list_jobs(&self, request: ListJobsParams) -> JobExecResult<ListJobsResponse>;

    async fn read_jobs(&self, request: ReadJobsParams) -> JobExecResult<ReadJobsResponse>;

    async fn cancel_jobs(&self, request: CancelJobsParams) -> JobExecResult<CancelJobsResponse>;
}

#[derive(Debug, Error)]
pub enum JobError {
    #[error("environment jobs unsupported: {message}")]
    Unsupported { message: String },

    #[error("invalid environment job request: {message}")]
    InvalidRequest { message: String },

    #[error("environment job execution failed: {message}")]
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandleArg {
    pub instance_id: String,
    pub job_id: JobId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandle {
    pub instance_id: String,
    pub job_id: JobId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStartArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
    pub jobs: Vec<JobStartSpecArgs>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStartSpecArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<FsPath>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<JobDependency>,
    #[serde(default)]
    pub dependency_policy: JobDependencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

impl JobStartSpecArgs {
    pub fn into_host_spec(self, job_id: JobId) -> JobExecResult<JobStartSpec> {
        if self.argv.is_empty() {
            return Err(JobError::InvalidRequest {
                message: "job argv must not be empty".to_owned(),
            });
        }
        Ok(JobStartSpec {
            job_id,
            name: self.name,
            argv: self.argv,
            cwd: self.cwd.as_ref().map(host_path).transpose()?,
            env: self.env,
            secret_env: BTreeMap::new(),
            stdin: self.stdin.map(|value| ByteChunk::from(value.into_bytes())),
            timeout_ms: self.timeout_ms,
            depends_on: self.depends_on,
            dependency_policy: self.dependency_policy,
            queue_key: self.queue_key,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobReadArgs {
    pub jobs: Vec<JobHandleArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub include_artifacts: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelArgs {
    pub jobs: Vec<JobHandleArg>,
    #[serde(default)]
    pub scope: JobCancelScope,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStartResult {
    pub jobs: Vec<JobStarted>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStarted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    pub status: JobStatus,
    pub dependencies: Vec<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
    /// Promise settled when this durable job reaches a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobReadResultEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<JobSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_chunks: Vec<JobOutputChunk>,
    pub output_next_seq: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobReadResultSet {
    pub jobs: Vec<JobReadResultEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobListResultSet {
    pub jobs: Vec<JobListResultEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobListResultEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<JobSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelResultSet {
    pub jobs: Vec<JobCancelResultEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancelResultEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<JobSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn is_environment_job_tool_name(name: &str) -> bool {
    matches!(
        name,
        JOB_START_TOOL_NAME | JOB_LIST_TOOL_NAME | JOB_READ_TOOL_NAME
    )
}

pub fn visible_job_read_output(jobs: &[JobReadResultEntry]) -> String {
    let mut lines = Vec::new();
    for job in jobs {
        if let Some(error) = &job.error {
            let label = job
                .handle
                .as_ref()
                .map(|handle| handle.job_id.as_str())
                .unwrap_or("<unknown>");
            lines.push(format!("{label}: error: {error}"));
            continue;
        }
        let Some(summary) = &job.summary else {
            continue;
        };
        lines.push(format!("{}: {:?}", summary.job_id.as_str(), summary.status));
        let tail = visible_output_chunks(&job.output_chunks);
        if !tail.is_empty() {
            lines.push(tail);
        }
    }
    lines.join("\n")
}

pub fn visible_job_list_output(jobs: &[JobListResultEntry]) -> String {
    let mut lines = Vec::new();
    for job in jobs {
        if let Some(error) = &job.error {
            let label = job
                .handle
                .as_ref()
                .map(|handle| handle.job_id.as_str())
                .unwrap_or("<unknown>");
            lines.push(format!("{label}: error: {error}"));
            continue;
        }
        let Some(summary) = &job.summary else {
            continue;
        };
        lines.push(format!("{}: {:?}", summary.job_id.as_str(), summary.status));
    }
    lines.join("\n")
}

pub fn visible_output_chunks(chunks: &[JobOutputChunk]) -> String {
    chunks
        .iter()
        .filter_map(|chunk| String::from_utf8(chunk.chunk.clone().into_inner()).ok())
        .collect::<Vec<_>>()
        .join("")
}

fn host_path(path: &FsPath) -> JobExecResult<HostPath> {
    HostPath::new(path.as_str()).map_err(|error| JobError::InvalidRequest {
        message: error.to_string(),
    })
}
