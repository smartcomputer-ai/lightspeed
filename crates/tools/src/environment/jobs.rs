//! Durable environment job capability boundary.
//!
//! The model-facing argument and result types here are this crate's own:
//! they map to and from the environment protocol's job wire types at this
//! boundary, so a protocol change never silently reshapes what the model
//! reads or writes.

use std::collections::BTreeMap;

use async_trait::async_trait;
use environment_protocol::{
    bytes::ByteChunk,
    data::{
        exec::{CommandSpec, ExecInput, ExitStatus, StoppedBy},
        handshake::JobLimits,
        job::{
            CancelJobsParams, CancelJobsResponse, JobDependency, JobDependencyPolicy, JobReadEntry,
            JobReadRequest, JobSpec, JobStatus, JobSummary, ReadJobsParams, ReadJobsResponse,
            StartJobsParams, StartJobsResponse,
        },
    },
    ids::{JobId, JobName, QueueKey},
    paths::EnvironmentPath,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use harness::{
    BlobRef,
    storage::{BlobEdge, BlobGraphStore, BlobStore, BlobStoreError},
};

use crate::{
    environment::{
        output::{ShownOutput, is_stderr},
        process::LeftoverProcess,
    },
    fs::FsPath,
};

pub const JOB_SUBMIT_TOOL_NAME: &str = "job_submit";
pub const JOB_RUN_TOOL_NAME: &str = "job_run";
pub const JOB_READ_TOOL_NAME: &str = "job_read";
pub const JOB_SUBMIT_WORKFLOW_TOOL_ID: &str = "environment-job-submit";
pub const JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE: &str = "lightspeed.environment.job.submit.v1";
pub const JOB_RUN_WORKFLOW_TOOL_ID: &str = "environment-job-run";
pub const JOB_RUN_WORKFLOW_SEMANTIC_TYPE: &str = "lightspeed.environment.job.run.v1";
pub const JOB_RUN_DEFAULT_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
pub const JOB_RUN_MAX_TIMEOUT_MS: u64 = 60 * 60 * 1_000;
pub const JOB_RUN_DEADLINE_AFTER_MS: u64 = 65 * 60 * 1_000;
/// Output a job result shows when the reader names no budget: the first
/// half from the start of the output, the second half from its end.
pub const DEFAULT_JOB_OUTPUT_BYTES: u64 = 16 * 1024;

pub type JobExecResult<T> = Result<T, JobError>;

#[async_trait]
pub trait JobExecutor: Send + Sync {
    /// The endpoint's job ceilings, when known; reads stay within them.
    fn limits(&self) -> Option<JobLimits> {
        None
    }

    async fn start_jobs(&self, request: StartJobsParams) -> JobExecResult<StartJobsResponse>;

    async fn read_jobs(&self, request: ReadJobsParams) -> JobExecResult<ReadJobsResponse>;

    async fn cancel_jobs(&self, request: CancelJobsParams) -> JobExecResult<CancelJobsResponse>;
}

#[derive(Debug, Error)]
pub enum JobError {
    #[error("environment jobs unsupported: {message}")]
    Unsupported { message: String },

    #[error("invalid environment job request: {message}")]
    InvalidRequest { message: String },

    /// A job id is taken by another run; the model must choose a new id.
    #[error("environment job id in use: {message}")]
    JobIdInUse { message: String },

    #[error("environment job execution failed: {message}")]
    Failed { message: String },
}

/// A job as the model names it. `environment_id` defaults to the session's
/// active environment — the one `job_submit` targets — and is only needed
/// to read a job in another environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandleArg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    pub job_id: JobId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobHandle {
    pub environment_id: String,
    #[schemars(with = "String")]
    pub job_id: JobId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSubmitArgs {
    pub jobs: Vec<JobSubmitSpecArgs>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRunArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<FsPath>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

impl JobRunArgs {
    /// The wire spec of this single job. A relative `cwd` resolves against
    /// `working_directory`, which is also the default `cwd`.
    pub fn into_job_spec(
        self,
        job_id: JobId,
        working_directory: Option<&str>,
    ) -> JobExecResult<JobSpec> {
        let timeout_ms = self.timeout_ms.unwrap_or(JOB_RUN_DEFAULT_TIMEOUT_MS);
        if timeout_ms > JOB_RUN_MAX_TIMEOUT_MS {
            return Err(JobError::InvalidRequest {
                message: format!("job_run timeout_ms must be at most {JOB_RUN_MAX_TIMEOUT_MS}"),
            });
        }
        JobSubmitSpecArgs {
            name: self.name,
            job_id: job_id.clone(),
            argv: self.argv,
            cwd: self.cwd,
            env: self.env,
            stdin: self.stdin,
            timeout_ms: Some(timeout_ms),
            depends_on: Vec::new(),
            dependency_policy: JobDependencyPolicyArg::AllSucceeded,
            queue_key: self.queue_key,
        }
        .into_job_spec(job_id, working_directory)
    }
}

/// Runtime-owned facts pinned when a durable `job_submit` call is accepted.
/// The receiving workflow reads this through the generic invocation's opaque
/// execution-context reference; it is not part of the model-facing schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSubmitExecutionContextV1 {
    pub version: u32,
    /// The active attachment's working directory at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// The active environment at admission; the executor has already
    /// checked that its attachment grants durable jobs.
    pub environment_id: String,
}

impl JobSubmitExecutionContextV1 {
    pub const VERSION: u32 = 2;

    pub fn new(environment_id: String, working_directory: Option<String>) -> Self {
        Self {
            version: Self::VERSION,
            working_directory,
            environment_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDependencyArg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl JobDependencyArg {
    fn into_job_dependency(self) -> JobExecResult<JobDependency> {
        match (self.job_id, self.name) {
            (Some(job_id), None) => Ok(JobDependency::JobId { job_id }),
            (None, Some(name)) if !name.is_empty() => Ok(JobDependency::Name {
                name: job_name(name)?,
            }),
            (Some(_), Some(_)) => Err(JobError::InvalidRequest {
                message: "job dependency must specify job_id or name, not both".to_owned(),
            }),
            _ => Err(JobError::InvalidRequest {
                message: "job dependency must specify job_id or name".to_owned(),
            }),
        }
    }
}

/// What a job waits for from its `depends_on` jobs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobDependencyPolicyArg {
    /// Run once every dependency succeeded.
    #[default]
    AllSucceeded,
    /// Run once every dependency finished, whatever its outcome.
    AllTerminal,
}

impl From<JobDependencyPolicyArg> for JobDependencyPolicy {
    fn from(policy: JobDependencyPolicyArg) -> Self {
        match policy {
            JobDependencyPolicyArg::AllSucceeded => Self::AllSucceeded,
            JobDependencyPolicyArg::AllTerminal => Self::AllTerminal,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSubmitSpecArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub job_id: JobId,
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
    pub depends_on: Vec<JobDependencyArg>,
    #[serde(default)]
    pub dependency_policy: JobDependencyPolicyArg,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

impl JobSubmitSpecArgs {
    /// The wire spec of this job. A relative `cwd` resolves against
    /// `working_directory`, which is also the default `cwd`; without one a
    /// relative `cwd` is refused and an absent one means the environment's
    /// default.
    pub fn into_job_spec(
        self,
        job_id: JobId,
        working_directory: Option<&str>,
    ) -> JobExecResult<JobSpec> {
        if self.argv.is_empty() {
            return Err(JobError::InvalidRequest {
                message: "job argv must not be empty".to_owned(),
            });
        }
        // The model's job id keys the job's promise, so it must fit the
        // completion-key shape; checking here keeps key derivation from
        // ever failing on an accepted job.
        harness::validate_completion_key(job_id.as_str()).map_err(|error| {
            JobError::InvalidRequest {
                message: format!("job_id {:?} is invalid: {error}", job_id.as_str()),
            }
        })?;
        let depends_on = self
            .depends_on
            .into_iter()
            .map(JobDependencyArg::into_job_dependency)
            .collect::<JobExecResult<Vec<_>>>()?;
        Ok(JobSpec {
            job_id,
            name: self.name.map(job_name).transpose()?,
            command: CommandSpec {
                argv: self.argv,
                cwd: job_cwd(self.cwd.as_ref(), working_directory)?,
                env: self.env,
                secret_env: BTreeMap::new(),
                input: match self.stdin {
                    Some(stdin) => ExecInput::Bytes {
                        data: ByteChunk::from(stdin.into_bytes()),
                    },
                    None => ExecInput::None {},
                },
                timeout_ms: self.timeout_ms,
            },
            depends_on,
            dependency_policy: self.dependency_policy.into(),
            queue_key: self
                .queue_key
                .map(|key| {
                    QueueKey::new(key).map_err(|error| JobError::InvalidRequest {
                        message: format!("queue_key is invalid: {error}"),
                    })
                })
                .transpose()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobReadArgs {
    pub jobs: Vec<JobHandleArg>,
    /// Output bytes to show per job. Without `after`, half come from the
    /// start of the output and half from its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// Byte offset to page forward from, such as a previous result's
    /// `output_next` or its omitted range's `offset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobSubmitResult {
    pub jobs: Vec<JobSubmitted>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobSubmitted {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[schemars(with = "String")]
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    pub status: ModelJobStatus,
    #[schemars(with = "Vec<String>")]
    pub dependencies: Vec<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
    /// Promise settled when this durable job reaches a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise: Option<String>,
}

impl JobSubmitted {
    pub fn from_summary(summary: &JobSummary, handle: Option<JobHandle>) -> Self {
        Self {
            name: summary.name.as_ref().map(|name| name.as_str().to_owned()),
            job_id: summary.job_id.clone(),
            handle,
            status: summary.status.into(),
            dependencies: summary.depends_on.clone(),
            queue_key: summary
                .queue_key
                .as_ref()
                .map(|key| key.as_str().to_owned()),
            promise: None,
        }
    }
}

/// Where a job stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelJobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    DependencyFailed,
    Interrupted,
}

impl ModelJobStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

impl From<JobStatus> for ModelJobStatus {
    fn from(status: JobStatus) -> Self {
        match status {
            JobStatus::Queued => Self::Queued,
            JobStatus::Running => Self::Running,
            JobStatus::Succeeded => Self::Succeeded,
            JobStatus::Failed => Self::Failed,
            JobStatus::TimedOut => Self::TimedOut,
            JobStatus::Cancelled => Self::Cancelled,
            JobStatus::DependencyFailed => Self::DependencyFailed,
            JobStatus::Interrupted => Self::Interrupted,
        }
    }
}

/// Why the environment stopped a job that did not end on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelStoppedBy {
    Timeout,
    Terminate,
    Kill,
}

impl From<StoppedBy> for ModelStoppedBy {
    fn from(stopped_by: StoppedBy) -> Self {
        match stopped_by {
            StoppedBy::Timeout => Self::Timeout,
            StoppedBy::Terminate => Self::Terminate,
            StoppedBy::Kill => Self::Kill,
        }
    }
}

/// What a job is and how it ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelJobSummary {
    #[schemars(with = "String")]
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: ModelJobStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(with = "Vec<String>")]
    pub depends_on: Vec<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The signal that killed the job's command, such as `SIGSEGV`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub core_dumped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<ModelStoppedBy>,
    /// Processes the job left running, which the environment stopped when
    /// the job finished; the job's script should shut them down itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub swept_processes: Vec<LeftoverProcess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<JobSummary> for ModelJobSummary {
    fn from(summary: JobSummary) -> Self {
        let (exit_code, signal, core_dumped) = match summary.exit {
            Some(ExitStatus::Code { code }) => (Some(code), None, false),
            Some(ExitStatus::Signal {
                signal,
                core_dumped,
            }) => (None, Some(signal.as_str().to_owned()), core_dumped),
            None => (None, None, false),
        };
        Self {
            job_id: summary.job_id,
            name: summary.name.map(JobName::into_string),
            status: summary.status.into(),
            depends_on: summary.depends_on,
            queue_key: summary.queue_key.map(QueueKey::into_string),
            created_at_ms: summary.created_at_ms,
            started_at_ms: summary.started_at_ms,
            finished_at_ms: summary.finished_at_ms,
            duration_ms: summary.duration_ms,
            exit_code,
            signal,
            core_dumped,
            stopped_by: summary.stopped_by.map(Into::into),
            swept_processes: summary
                .leftovers
                .map(|report| {
                    report
                        .processes
                        .into_iter()
                        .map(|process| LeftoverProcess {
                            pid: process.pid,
                            command: process.command.unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            error: summary.error.map(|error| error.to_string()),
        }
    }
}

/// A range of output the result leaves out. Read it with `job_read`
/// `after: offset`; the `lost_bytes` of it that the environment dropped to
/// bound retained output read back as a gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelOutputOmission {
    /// Byte offset of the first omitted byte.
    pub offset: u64,
    pub bytes: u64,
    /// Omitted bytes that are gone for good; reading them again does not
    /// return them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lost_bytes: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelJobResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ModelJobSummary>,
    /// Output from the requested position, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<ModelJobOutputSegment>,
    /// Output between `output` and `tail` that is not shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<ModelOutputOmission>,
    /// The last output bytes, after the omitted range.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<ModelJobOutputSegment>,
    /// Byte offset after the last output shown; pass it as `after` to read
    /// on.
    pub output_next: u64,
    /// Output bytes the job produced so far.
    pub output_end: u64,
    /// More output exists after `output_next`.
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelJobResultSet {
    pub jobs: Vec<ModelJobResult>,
}

/// Which stream produced an output segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelOutputStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelJobOutputSegment {
    pub stream: ModelOutputStream,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<usize>,
}

pub fn visible_job_read_output(jobs: &[ModelJobResult]) -> String {
    serde_json::to_string_pretty(&ModelJobResultSet {
        jobs: jobs.to_vec(),
    })
    .unwrap_or_else(|error| format!("failed to encode job results: {error}"))
}

/// The `job/read` entries that show one job: a page forward from `after`,
/// or, without it, the head and the tail of the output, each half of
/// `output_bytes`. [`NormalizeJobResultInput::from_entries`] reassembles the
/// answer.
pub fn job_output_requests(
    job_id: &JobId,
    after: Option<u64>,
    output_bytes: u64,
) -> Vec<JobReadRequest> {
    match after {
        Some(after) => vec![JobReadRequest {
            job_id: job_id.clone(),
            after: Some(after),
            tail_bytes: None,
            max_bytes: Some(output_bytes),
        }],
        None => {
            let head = output_bytes / 2;
            vec![
                JobReadRequest {
                    job_id: job_id.clone(),
                    after: Some(0),
                    tail_bytes: None,
                    max_bytes: Some(head),
                },
                JobReadRequest {
                    job_id: job_id.clone(),
                    after: None,
                    tail_bytes: Some(output_bytes - head),
                    max_bytes: Some(output_bytes - head),
                },
            ]
        }
    }
}

/// A complete `job/read` for `jobs`, each shown as [`job_output_requests`]
/// describes, waiting for nothing.
pub fn job_output_read(jobs: &[JobId], after: Option<u64>, output_bytes: u64) -> ReadJobsParams {
    let entries = jobs
        .iter()
        .flat_map(|job_id| job_output_requests(job_id, after, output_bytes))
        .collect::<Vec<_>>();
    ReadJobsParams {
        max_bytes: output_bytes.saturating_mul(jobs.len() as u64),
        jobs: entries,
        wait_ms: 0,
        return_on: environment_protocol::data::exec::ReturnOn::AnyOutput,
    }
}

#[derive(Clone, Debug, Default)]
pub struct NormalizeJobResultInput {
    pub handle: Option<JobHandle>,
    pub summary: Option<JobSummary>,
    /// Output shown from the requested position: `output`, then `omitted`
    /// and `tail`.
    pub output: ShownOutput,
    pub error: Option<String>,
}

impl NormalizeJobResultInput {
    /// Reassemble the entries [`job_output_requests`] asked for one job: a
    /// summary and its pages, or the entry's error (`notFound` for a job the
    /// environment does not know).
    pub fn from_entries(
        handle: Option<JobHandle>,
        entries: impl IntoIterator<Item = JobReadEntry>,
    ) -> Self {
        let mut summary = None;
        let mut pages = Vec::new();
        let mut error = None;
        for entry in entries {
            match entry {
                JobReadEntry::Job {
                    summary: entry_summary,
                    output,
                } => {
                    summary = Some(*entry_summary);
                    pages.push(output);
                }
                JobReadEntry::Error {
                    job_id,
                    error: cause,
                } => {
                    error.get_or_insert_with(|| format!("job {job_id}: {cause}"));
                }
            }
        }
        let mut pages = pages.into_iter();
        let output = pages
            .next()
            .map(|head| ShownOutput::from_pages(head, pages.next()))
            .unwrap_or_default();
        Self {
            handle,
            summary,
            output,
            error,
        }
    }
}

pub async fn normalize_job_result(
    blobs: &dyn BlobStore,
    input: NormalizeJobResultInput,
) -> Result<ModelJobResult, BlobStoreError> {
    let NormalizeJobResultInput {
        handle,
        summary,
        output,
        error,
    } = input;
    let truncated = output.next < output.end;
    let omitted = output.omitted.map(|omission| ModelOutputOmission {
        offset: omission.offset,
        bytes: omission.bytes,
        lost_bytes: omission.lost_bytes,
    });
    let head = segments(blobs, output.head).await?;
    let tail = segments(blobs, output.tail).await?;
    Ok(ModelJobResult {
        handle: handle.map(|mut handle| {
            handle.environment_id = super::handles::environment_handle(&handle.environment_id);
            handle
        }),
        summary: summary.map(Into::into),
        output: head,
        omitted,
        tail,
        output_next: output.next,
        output_end: output.end,
        truncated,
        error,
    })
}

/// A result that carries only an error, such as an unknown job.
pub fn model_job_error(handle: Option<JobHandle>, error: String) -> ModelJobResult {
    ModelJobResult {
        handle: handle.map(|mut handle| {
            handle.environment_id = super::handles::environment_handle(&handle.environment_id);
            handle
        }),
        summary: None,
        output: Vec::new(),
        omitted: None,
        tail: Vec::new(),
        output_next: 0,
        output_end: 0,
        truncated: false,
        error: Some(error),
    }
}

/// Merge consecutive chunks of one stream; text stays inline, bytes that
/// are not UTF-8 go to CAS and are referenced.
async fn segments(
    blobs: &dyn BlobStore,
    chunks: Vec<environment_protocol::data::exec::OutputChunk>,
) -> Result<Vec<ModelJobOutputSegment>, BlobStoreError> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < chunks.len() {
        let stream = model_stream(chunks[index].stream);
        let mut bytes = Vec::new();
        while index < chunks.len() && model_stream(chunks[index].stream) == stream {
            bytes.extend_from_slice(chunks[index].data.as_slice());
            index += 1;
        }
        match String::from_utf8(bytes) {
            Ok(text) => {
                if !text.is_empty() {
                    output.push(ModelJobOutputSegment {
                        stream,
                        text: Some(text),
                        blob_ref: None,
                        media_type: None,
                        byte_len: None,
                    });
                }
            }
            Err(error) => {
                let bytes = error.into_bytes();
                let byte_len = bytes.len();
                let blob_ref = blobs.put_bytes(bytes).await?;
                output.push(ModelJobOutputSegment {
                    stream,
                    text: None,
                    blob_ref: Some(blob_ref),
                    media_type: Some("application/octet-stream".to_owned()),
                    byte_len: Some(byte_len),
                });
            }
        }
    }
    Ok(output)
}

fn model_stream(stream: environment_protocol::data::exec::OutputStream) -> ModelOutputStream {
    if is_stderr(stream) {
        ModelOutputStream::Stderr
    } else {
        ModelOutputStream::Stdout
    }
}

pub async fn store_model_job_result(
    blobs: &dyn BlobStore,
    blob_graph: Option<&dyn BlobGraphStore>,
    result: &ModelJobResult,
) -> Result<BlobRef, BlobStoreError> {
    let root = blobs
        .put_bytes(
            serde_json::to_vec(result).map_err(|error| BlobStoreError::Store {
                message: error.to_string(),
            })?,
        )
        .await?;
    let edges = result
        .output
        .iter()
        .chain(&result.tail)
        .filter_map(|segment| {
            segment
                .blob_ref
                .clone()
                .map(|child| BlobEdge::contains(root.clone(), child))
        })
        .collect::<Vec<_>>();
    if !edges.is_empty()
        && let Some(blob_graph) = blob_graph
    {
        blob_graph.record_blob_edges(edges).await?;
    }
    Ok(root)
}

fn job_name(name: String) -> JobExecResult<JobName> {
    JobName::new(name).map_err(|error| JobError::InvalidRequest {
        message: format!("job name is invalid: {error}"),
    })
}

fn job_cwd(
    cwd: Option<&FsPath>,
    working_directory: Option<&str>,
) -> JobExecResult<Option<EnvironmentPath>> {
    let invalid = |message: String| JobError::InvalidRequest { message };
    let resolved = match (cwd, working_directory) {
        (Some(cwd), _) if cwd.is_absolute() => cwd.as_str().to_owned(),
        (cwd, Some(base)) => crate::environment::sources::absolute(
            std::path::Path::new(base),
            cwd.map_or(".", FsPath::as_str),
        )
        .map_err(invalid)?
        .to_string_lossy()
        .into_owned(),
        (Some(cwd), None) => {
            return Err(invalid(format!(
                "job cwd {cwd} is relative and the environment has no working directory"
            )));
        }
        (None, None) => return Ok(None),
    };
    EnvironmentPath::new(resolved)
        .map(Some)
        .map_err(|error| invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use environment_protocol::data::exec::{OutputChunk, OutputPage, OutputStream, SignalName};
    use harness::storage::{
        BlobEdge, BlobGraphStore, BlobStore, BlobStoreError, InMemoryBlobStore,
    };
    use serde_json::json;

    use super::*;

    fn job_id(value: &str) -> JobId {
        JobId::new(value).unwrap()
    }

    #[test]
    fn job_submit_arguments_do_not_accept_an_environment_override() {
        let error = serde_json::from_value::<JobSubmitArgs>(json!({
            "environment_id": "environment-other",
            "jobs": []
        }))
        .expect_err("environment override must not be model-facing");

        assert!(error.to_string().contains("unknown field `environment_id`"));
    }

    #[test]
    fn job_run_is_single_flat_work_with_a_runtime_job_id() {
        let args = serde_json::from_value::<JobRunArgs>(json!({
            "name": "tests",
            "argv": ["cargo", "test"]
        }))
        .expect("decode job_run");
        let spec = args
            .into_job_spec(job_id("job-derived"), None)
            .expect("materialize job_run");

        assert_eq!(spec.job_id, job_id("job-derived"));
        assert_eq!(spec.command.argv, vec!["cargo", "test"]);
        assert_eq!(spec.command.timeout_ms, Some(JOB_RUN_DEFAULT_TIMEOUT_MS));
        assert!(spec.depends_on.is_empty());
    }

    #[test]
    fn job_cwd_resolves_against_the_working_directory() {
        let spec = |cwd: Option<&str>, base: Option<&str>| {
            JobSubmitSpecArgs {
                name: None,
                job_id: job_id("build"),
                argv: vec!["true".to_owned()],
                cwd: cwd.map(|cwd| FsPath::new(cwd).unwrap()),
                env: BTreeMap::new(),
                stdin: Some("input".to_owned()),
                timeout_ms: None,
                depends_on: Vec::new(),
                dependency_policy: JobDependencyPolicyArg::AllSucceeded,
                queue_key: None,
            }
            .into_job_spec(job_id("build"), base)
        };
        let resolved = spec(Some("sub"), Some("/srv")).unwrap();
        assert_eq!(resolved.command.cwd.unwrap().as_str(), "/srv/sub");
        assert_eq!(
            resolved.command.input,
            ExecInput::Bytes {
                data: b"input".to_vec().into()
            }
        );
        assert_eq!(
            spec(None, Some("/srv"))
                .unwrap()
                .command
                .cwd
                .unwrap()
                .as_str(),
            "/srv"
        );
        assert_eq!(
            spec(Some("/abs"), Some("/srv"))
                .unwrap()
                .command
                .cwd
                .unwrap()
                .as_str(),
            "/abs"
        );
        assert!(spec(None, None).unwrap().command.cwd.is_none());
        assert!(matches!(
            spec(Some("sub"), None),
            Err(JobError::InvalidRequest { .. })
        ));
    }

    #[test]
    fn job_submit_accepts_snake_case_job_id_dependencies() {
        let args = serde_json::from_value::<JobSubmitArgs>(json!({
            "jobs": [{
                "job_id": "build",
                "argv": ["true"]
            }, {
                "job_id": "test",
                "argv": ["true"],
                "depends_on": [{"job_id": "build"}, {"name": "lint"}],
                "dependency_policy": "allTerminal"
            }]
        }))
        .expect("decode job_submit with snake_case dependency id");

        let specs = args
            .jobs
            .into_iter()
            .map(|spec| {
                let job_id = spec.job_id.clone();
                spec.into_job_spec(job_id, None)
            })
            .collect::<JobExecResult<Vec<_>>>()
            .expect("materialize protocol specs");

        assert_eq!(
            specs[1].depends_on,
            vec![
                JobDependency::JobId {
                    job_id: job_id("build")
                },
                JobDependency::Name {
                    name: JobName::new("lint").unwrap()
                }
            ]
        );
        assert_eq!(specs[1].dependency_policy, JobDependencyPolicy::AllTerminal);
    }

    #[test]
    fn job_submit_rejects_empty_dependencies_before_environment_submission() {
        let args = serde_json::from_value::<JobSubmitArgs>(json!({
            "jobs": [{
                "job_id": "test",
                "argv": ["true"],
                "depends_on": [{}]
            }]
        }))
        .expect("decode job_submit with empty dependency object");

        let spec = args.jobs.into_iter().next().expect("job");
        let job_id = spec.job_id.clone();
        let error = spec
            .into_job_spec(job_id, None)
            .expect_err("empty dependency must be rejected locally");

        assert!(matches!(error, JobError::InvalidRequest { .. }));
    }

    #[test]
    fn job_run_rejects_group_fields_and_excessive_timeout() {
        for arguments in [
            json!({"job_id": "model-owned", "argv": ["true"]}),
            json!({"argv": ["true"], "depends_on": []}),
            json!({"jobs": [{"job_id": "one", "argv": ["true"]}]}),
        ] {
            serde_json::from_value::<JobRunArgs>(arguments)
                .expect_err("job_run group fields must not decode");
        }

        let args = serde_json::from_value::<JobRunArgs>(json!({
            "argv": ["true"],
            "timeout_ms": JOB_RUN_MAX_TIMEOUT_MS + 1
        }))
        .expect("timeout range is a domain validation");
        let error = args
            .into_job_spec(job_id("job-derived"), None)
            .expect_err("timeout above maximum must fail");
        assert!(matches!(error, JobError::InvalidRequest { .. }));
    }

    #[test]
    fn job_ids_must_fit_the_completion_key_shape() {
        for value in ["build:1", "-lead", &"x".repeat(65)] {
            let Ok(id) = JobId::new(value) else {
                continue;
            };
            let error = JobSubmitSpecArgs {
                name: None,
                job_id: id.clone(),
                argv: vec!["true".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: None,
                depends_on: Vec::new(),
                dependency_policy: JobDependencyPolicyArg::AllSucceeded,
                queue_key: None,
            }
            .into_job_spec(id, None);
            assert!(
                matches!(error, Err(JobError::InvalidRequest { .. })),
                "{value:?} must be rejected"
            );
        }
    }

    #[test]
    fn job_submit_execution_context_is_versioned_and_runtime_owned() {
        let context =
            JobSubmitExecutionContextV1::new("environment-active".to_owned(), Some("/srv".into()));

        assert_eq!(context.version, JobSubmitExecutionContextV1::VERSION);
        assert_eq!(context.environment_id, "environment-active");
        assert_eq!(context.working_directory.as_deref(), Some("/srv"));
    }

    #[test]
    fn head_and_tail_requests_split_the_budget() {
        let requests = job_output_requests(&job_id("build"), None, 16);
        assert_eq!(requests.len(), 2);
        assert_eq!(
            (requests[0].after, requests[0].max_bytes),
            (Some(0), Some(8))
        );
        assert_eq!(
            (requests[1].tail_bytes, requests[1].max_bytes),
            (Some(8), Some(8))
        );
        let paged = job_output_requests(&job_id("build"), Some(40), 16);
        assert_eq!((paged[0].after, paged[0].max_bytes), (Some(40), Some(16)));
        let read = job_output_read(&[job_id("a"), job_id("b")], None, 16);
        assert_eq!(read.jobs.len(), 4);
        assert_eq!(read.max_bytes, 32);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_results_show_head_and_tail_with_the_skipped_count() {
        let blobs = InMemoryBlobStore::new();
        let entries = vec![
            JobReadEntry::Job {
                summary: Box::new(test_summary()),
                output: page(
                    vec![chunk(0, OutputStream::Stdout, b"compiling\n")],
                    10,
                    1000,
                ),
            },
            JobReadEntry::Job {
                summary: Box::new(test_summary()),
                output: page(
                    vec![chunk(990, OutputStream::Stderr, b"FAILED: 3\n")],
                    1000,
                    1000,
                ),
            },
        ];
        let result =
            normalize_job_result(&blobs, NormalizeJobResultInput::from_entries(None, entries))
                .await
                .expect("normalize");

        assert_eq!(result.output[0].text.as_deref(), Some("compiling\n"));
        assert_eq!(result.tail[0].text.as_deref(), Some("FAILED: 3\n"));
        assert_eq!(result.tail[0].stream, ModelOutputStream::Stderr);
        assert_eq!(
            result.omitted,
            Some(ModelOutputOmission {
                offset: 10,
                bytes: 980,
                lost_bytes: 0,
            })
        );
        assert!(!result.truncated);
        let value = serde_json::to_value(&result).expect("semantic JSON");
        assert_eq!(value["output_end"], 1000);
        assert_eq!(value["summary"]["status"], "failed");
        assert_eq!(value["summary"]["signal"], "SIGSEGV");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_jobs_read_as_errors() {
        let blobs = InMemoryBlobStore::new();
        let result = normalize_job_result(
            &blobs,
            NormalizeJobResultInput::from_entries(
                None,
                vec![JobReadEntry::Error {
                    job_id: job_id("gone"),
                    error: environment_protocol::error::ProtocolError::not_found("unknown job"),
                }],
            ),
        )
        .await
        .expect("normalize");
        assert!(result.summary.is_none());
        assert!(result.error.as_deref().is_some_and(|e| e.contains("gone")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalizer_reassembles_utf8_and_preserves_stream_order() {
        let blobs = InMemoryBlobStore::new();
        let result = normalize_job_result(
            &blobs,
            NormalizeJobResultInput {
                summary: Some(test_summary()),
                output: ShownOutput::from_pages(
                    page(
                        vec![
                            chunk(0, OutputStream::Stdout, &[b'a', 0xe2]),
                            chunk(2, OutputStream::Stdout, &[0x82, 0xac, b'\n']),
                            chunk(5, OutputStream::Stderr, b"warning\n"),
                            chunk(13, OutputStream::Stdout, b"done\n"),
                        ],
                        18,
                        18,
                    ),
                    None,
                ),
                ..Default::default()
            },
        )
        .await
        .expect("normalize text");

        assert_eq!(result.output.len(), 3);
        assert_eq!(result.output[0].text.as_deref(), Some("a€\n"));
        assert_eq!(result.output[1].stream, ModelOutputStream::Stderr);
        assert_eq!(result.output[2].text.as_deref(), Some("done\n"));
        assert!(!result.truncated);
        assert_eq!(result.output_next, 18);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalizer_stores_binary_output_as_a_ref() {
        let blobs = InMemoryBlobStore::new();
        let result = normalize_job_result(
            &blobs,
            NormalizeJobResultInput {
                summary: Some(test_summary()),
                output: ShownOutput::from_pages(
                    page(vec![chunk(0, OutputStream::Stdout, &[0xff, 0x00])], 2, 4),
                    None,
                ),
                ..Default::default()
            },
        )
        .await
        .expect("normalize binary");

        assert!(result.truncated, "output continues after output_next");
        let segment = &result.output[0];
        assert!(segment.text.is_none());
        assert_eq!(
            segment.media_type.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(segment.byte_len, Some(2));
        let blob_ref = segment.blob_ref.as_ref().expect("binary ref");
        assert_eq!(
            blobs.read_bytes(blob_ref).await.expect("binary bytes"),
            vec![0xff, 0x00]
        );
        let encoded = serde_json::to_string(&result).expect("semantic JSON");
        assert!(!encoded.contains("/wA="));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stored_job_manifest_records_binary_child_edges() {
        let blobs = InMemoryBlobStore::new();
        let graph = RecordingGraph::default();
        let result = normalize_job_result(
            &blobs,
            NormalizeJobResultInput {
                summary: Some(test_summary()),
                output: ShownOutput::from_pages(
                    page(vec![chunk(0, OutputStream::Stdout, &[0xff])], 1, 1),
                    None,
                ),
                ..Default::default()
            },
        )
        .await
        .expect("normalize binary");
        let child = result.output[0].blob_ref.clone().expect("binary child ref");
        let root = store_model_job_result(&blobs, Some(&graph), &result)
            .await
            .expect("store manifest");

        assert_eq!(
            graph.edges.lock().expect("edge lock").as_slice(),
            &[BlobEdge::contains(root, child)]
        );
    }

    #[derive(Default)]
    struct RecordingGraph {
        edges: Arc<Mutex<Vec<BlobEdge>>>,
    }

    #[async_trait]
    impl BlobGraphStore for RecordingGraph {
        async fn record_blob_edges(&self, edges: Vec<BlobEdge>) -> Result<(), BlobStoreError> {
            self.edges.lock().expect("edge lock").extend(edges);
            Ok(())
        }
    }

    fn chunk(offset: u64, stream: OutputStream, bytes: &[u8]) -> OutputChunk {
        OutputChunk {
            offset,
            stream,
            data: bytes.to_vec().into(),
        }
    }

    fn page(chunks: Vec<OutputChunk>, next: u64, end: u64) -> OutputPage {
        OutputPage {
            chunks,
            next,
            end,
            gaps: Vec::new(),
        }
    }

    fn test_summary() -> JobSummary {
        JobSummary {
            job_id: job_id("job-a"),
            request_id: environment_protocol::ids::JobRequestId::new("request-a").unwrap(),
            name: None,
            status: JobStatus::Failed,
            depends_on: Vec::new(),
            queue_key: None,
            created_at_ms: 1,
            queued_at_ms: Some(1),
            started_at_ms: Some(2),
            finished_at_ms: Some(3),
            cancel_requested_at_ms: None,
            duration_ms: Some(1),
            exit: Some(ExitStatus::Signal {
                signal: SignalName::new("SIGSEGV").unwrap(),
                core_dumped: false,
            }),
            stopped_by: None,
            leftovers: None,
            error: None,
        }
    }
}
