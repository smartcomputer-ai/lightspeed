//! `job/*`: durable batch commands with dependencies and queues.
//!
//! A job is identified by its `jobId` on the daemon together with the
//! `requestId` of the `job/start` that created it. Its record and output
//! survive daemon restarts; a job that was queued or running when the
//! daemon stopped is `interrupted`. When a job's root process finishes, the
//! daemon sweeps the job's process group and reports what it swept.

use serde::{Deserialize, Serialize};

use crate::{
    data::exec::{CommandSpec, ExitStatus, LeftoverReport, OutputPage, ReturnOn, StoppedBy},
    error::ProtocolError,
    ids::{JobId, JobName, JobRequestId, QueueKey},
    serde_util::{is_false, is_zero_u32},
};

/// `job/start` params. All-or-nothing: either every job is accepted (or
/// replayed) or none is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartJobsParams {
    /// Idempotency key of this start. Retrying with the same request id and
    /// identical jobs replays the existing jobs; reusing a job id with
    /// another request id or spec is `failedPrecondition` (`jobIdInUse`).
    pub request_id: JobRequestId,
    /// 1 to `limits.job.maxRequestJobs` jobs.
    pub jobs: Vec<JobSpec>,
}

/// One job of a `job/start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobSpec {
    pub job_id: JobId,
    /// Local name other jobs of the same `job/start` can depend on; unique
    /// within the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<JobName>,
    /// `input` must be `none` or `bytes`.
    pub command: CommandSpec,
    /// Jobs that must finish first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<JobDependency>,
    /// What "finish" means for `dependsOn`; `allSucceeded` by default.
    #[serde(default, skip_serializing_if = "JobDependencyPolicy::is_default")]
    pub dependency_policy: JobDependencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<QueueKey>,
}

/// A dependency reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum JobDependency {
    /// An existing job on this daemon, or one in the same request.
    JobId { job_id: JobId },
    /// The `name` of a job in the same `job/start`.
    Name { name: JobName },
}

/// What a job waits for from its `dependsOn` jobs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum JobDependencyPolicy {
    /// Run once every dependency `succeeded`; any other terminal status
    /// makes this job `dependencyFailed`.
    #[default]
    AllSucceeded,
    /// Run once every dependency is terminal, whatever its status.
    AllTerminal,
}

impl JobDependencyPolicy {
    pub fn is_default(&self) -> bool {
        *self == Self::AllSucceeded
    }
}

/// `job/start` response: one summary per requested job, in request order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartJobsResponse {
    /// Never empty.
    pub jobs: Vec<JobSummary>,
}

/// `job/read` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadJobsParams {
    /// 1 to `limits.job.maxRequestJobs` entries, each with its own position.
    pub jobs: Vec<JobReadRequest>,
    /// Most `data` bytes across all entries, at most
    /// `limits.job.maxReadBytes`, filled in request order: an entry reached
    /// after the budget is spent returns its summary and an empty page at
    /// its start position. 0 reads only summaries.
    pub max_bytes: u64,
    /// How long to wait, at most `limits.maxWaitMs`. 0 polls.
    pub wait_ms: u64,
    /// `anyOutput`: return when any entry has output past its position or
    /// changed status; `exit`: when every entry is terminal; `window`: wait
    /// the whole window.
    pub return_on: ReturnOn,
}

/// One entry of a `job/read`. Job reads never consume: without `after` and
/// `tailBytes` the page starts at offset 0. One job may be named by several
/// entries, such as a head and a tail page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobReadRequest {
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// Start at `end - tailBytes`; excludes `after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_bytes: Option<u64>,
    /// Most `data` bytes for this entry; the request's `maxBytes` still
    /// bounds the total. Absent: whatever of the request's budget remains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

/// `job/read` response: one entry per requested job, in request order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadJobsResponse {
    /// Never empty.
    pub jobs: Vec<JobReadEntry>,
}

/// A `job/read` entry: the job's summary and output page, or why it could
/// not be read (`notFound` for an unknown or garbage-collected job).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum JobReadEntry {
    Job {
        summary: Box<JobSummary>,
        output: OutputPage,
    },
    Error {
        job_id: JobId,
        error: ProtocolError,
    },
}

/// `job/cancel` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelJobsParams {
    /// 1 to `limits.job.maxRequestJobs` ids.
    pub job_ids: Vec<JobId>,
    /// `job` by default.
    #[serde(default, skip_serializing_if = "JobCancelScope::is_default")]
    pub scope: JobCancelScope,
    /// Kill at once instead of `SIGTERM`, `limits.job.cancelGraceMs`, then
    /// `SIGKILL`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

/// Which jobs a `job/cancel` affects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum JobCancelScope {
    /// Only the named jobs.
    #[default]
    Job,
    /// The named jobs and every job that transitively depends on them.
    Dependents,
}

impl JobCancelScope {
    pub fn is_default(&self) -> bool {
        *self == Self::Job
    }
}

/// `job/cancel` response: one entry per affected job, named jobs first in
/// request order, then dependents, at most `limits.job.maxRequestJobs`
/// entries in total.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelJobsResponse {
    /// Never empty.
    pub jobs: Vec<JobSummaryEntry>,
    /// Dependents cancelled as well but not listed, beyond the entry limit.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub unlisted_job_count: u32,
}

/// A job summary, or why the named job could not be found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum JobSummaryEntry {
    Job { summary: Box<JobSummary> },
    Error { job_id: JobId, error: ProtocolError },
}

/// What a job is and how it ended. Instants are the guest's wall clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobSummary {
    pub job_id: JobId,
    pub request_id: JobRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<JobName>,
    pub status: JobStatus,
    /// Resolved ids of the jobs this one depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<QueueKey>,
    pub created_at_ms: i64,
    /// When the job became runnable and waited for its queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    /// When a `job/cancel` reached the job while it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at_ms: Option<i64>,
    /// Monotonic run time from start to finish, immune to guest clock jumps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// How the root process ended, once it ran and exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitStatus>,
    /// Set when the daemon stopped the running job: `timeout` for
    /// `timedOut`, `terminate` or `kill` for a cancel without or with
    /// `force`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<StoppedBy>,
    /// What the daemon swept from the job's group at completion
    /// (`disposition: swept`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leftovers: Option<LeftoverReport>,
    /// Why a `failed` or `interrupted` job produced no exit, or an
    /// infrastructure failure while it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// Job lifecycle. `queued` and `running` are live; the rest are terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    /// Accepted; waiting for dependencies or its queue.
    Queued,
    Running,
    /// Exited with code 0.
    Succeeded,
    /// Exited otherwise, or could not start.
    Failed,
    /// Its `timeoutMs` expired and the daemon stopped it.
    TimedOut,
    /// Stopped or never started because of `job/cancel`.
    Cancelled,
    /// A dependency did not finish as the dependency policy requires.
    DependencyFailed,
    /// The daemon stopped while the job was queued or running.
    Interrupted,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}
