//! Records and activity contracts of `EnvironmentJobWorkflow`.
//!
//! The workflow owns every type it persists in history (arguments,
//! continue-as-new state, local-activity results, signals and the start
//! payload). None of them is an environment protocol wire type: the job
//! activities map between these records and `job/*` at their boundary, so a
//! protocol change never changes a history shape.

use std::collections::BTreeMap;

use harness::{BlobRef, PromiseSourceCheckResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::WorkflowToolStartArgs;

/// A job id on the environment's daemon (`jobId` on the wire). The start
/// activity validates it when it maps the payload onto `job/start`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvironmentJobId(String);

impl EnvironmentJobId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for EnvironmentJobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lifecycle of one job as the workflow records it. Every daemon status has
/// a variant; `lost` is the workflow's own outcome for a job the daemon no
/// longer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentJobStatus {
    /// Waiting for dependencies or its queue.
    Queued,
    Running,
    /// Exited with code 0.
    Succeeded,
    /// Exited otherwise, or could not start.
    Failed,
    /// Its timeout expired and the daemon stopped it.
    TimedOut,
    /// Stopped or never started because of a cancel.
    Cancelled,
    /// A dependency did not finish as the dependency policy requires.
    DependencyFailed,
    /// The daemon stopped while the job was queued or running.
    Interrupted,
    /// The daemon answered `notFound` for the job: it never existed there or
    /// its record was garbage-collected.
    Lost,
}

impl EnvironmentJobStatus {
    /// `queued` and `running` are live; every other status is final.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    pub fn is_success(self) -> bool {
        self == Self::Succeeded
    }
}

/// What the workflow keeps about one job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentJobRecord {
    pub job_id: EnvironmentJobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: EnvironmentJobStatus,
    /// Resolved ids of the jobs this one depends on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<EnvironmentJobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

impl EnvironmentJobRecord {
    /// The record of a job the daemon answered `notFound` for and the
    /// workflow holds nothing else about.
    pub fn lost(job_id: EnvironmentJobId) -> Self {
        Self {
            job_id,
            name: None,
            status: EnvironmentJobStatus::Lost,
            depends_on: Vec::new(),
            queue_key: None,
        }
    }
}

/// One job as a poll or cancel activity observed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum EnvironmentJobObservation {
    /// The daemon's summary, mapped onto the workflow's record.
    Job { record: EnvironmentJobRecord },
    /// The daemon answered this entry with `notFound`. The workflow records
    /// the job as `lost`, a terminal failure.
    NotFound { job_id: EnvironmentJobId },
}

impl EnvironmentJobObservation {
    pub fn job_id(&self) -> &EnvironmentJobId {
        match self {
            Self::Job { record } => &record.job_id,
            Self::NotFound { job_id } => job_id,
        }
    }
}

/// Which jobs a cancel affects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentJobCancelScope {
    /// Only the named jobs.
    #[default]
    Job,
    /// The named jobs and every job that transitively depends on them.
    Dependents,
}

/// Start payload of a job group, stored in CAS and named by
/// [`EnvironmentJobStartActivityRequest::request_ref`]. Written once by the
/// starter, read by the start activity, which resolves credential-bound
/// secret variables and maps it onto `job/start`. It never holds secret
/// values. Its content address is the stable fingerprint of the request, so
/// a starter derives the job-group id (and with it the workflow id) from the
/// encoded payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentJobStartPayloadV1 {
    /// [`Self::VERSION`].
    pub version: u32,
    /// Idempotency key of the start (`requestId` on the wire): a retried
    /// start with the same id and jobs replays the existing jobs.
    pub request_id: String,
    /// Never empty.
    pub jobs: Vec<EnvironmentJobSpec>,
}

impl EnvironmentJobStartPayloadV1 {
    pub const VERSION: u32 = 1;

    pub fn new(request_id: impl Into<String>, jobs: Vec<EnvironmentJobSpec>) -> Self {
        Self {
            version: Self::VERSION,
            request_id: request_id.into(),
            jobs,
        }
    }

    /// The bytes a starter stores in CAS.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Decode a stored payload, checking its version before its shape.
    pub fn decode(bytes: &[u8]) -> Result<Self, EnvironmentJobStartPayloadError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        let probe: VersionProbe =
            serde_json::from_slice(bytes).map_err(EnvironmentJobStartPayloadError::Decode)?;
        if probe.version != Self::VERSION {
            return Err(EnvironmentJobStartPayloadError::UnsupportedVersion {
                version: probe.version,
            });
        }
        serde_json::from_slice(bytes).map_err(EnvironmentJobStartPayloadError::Decode)
    }

    pub fn job_ids(&self) -> Vec<EnvironmentJobId> {
        self.jobs.iter().map(|job| job.job_id.clone()).collect()
    }
}

/// Why a stored start payload could not be read.
#[derive(Debug)]
pub enum EnvironmentJobStartPayloadError {
    Decode(serde_json::Error),
    UnsupportedVersion { version: u32 },
}

impl std::fmt::Display for EnvironmentJobStartPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "decode environment job start payload: {error}"),
            Self::UnsupportedVersion { version } => write!(
                f,
                "unsupported environment job start payload version {version} (expected {})",
                EnvironmentJobStartPayloadV1::VERSION
            ),
        }
    }
}

impl std::error::Error for EnvironmentJobStartPayloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            Self::UnsupportedVersion { .. } => None,
        }
    }
}

/// One job of a start payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentJobSpec {
    pub job_id: EnvironmentJobId,
    /// Local name other jobs of the same start can depend on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub command: EnvironmentJobCommand,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<EnvironmentJobDependency>,
    #[serde(
        default,
        skip_serializing_if = "EnvironmentJobDependencyPolicy::is_default"
    )]
    pub dependency_policy: EnvironmentJobDependencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

/// What a job runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentJobCommand {
    /// Program and arguments. Not empty.
    pub argv: Vec<String>,
    /// Absolute working directory in the environment; the daemon's default
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Plain variables. Credential-bound variables are resolved by the start
    /// activity and never stored here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Text written to standard input, then end of file; `/dev/null` when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// A dependency of a job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum EnvironmentJobDependency {
    /// An existing job on the daemon, or one in the same start.
    JobId { job_id: EnvironmentJobId },
    /// The `name` of a job in the same start.
    Name { name: String },
}

/// What a job waits for from its dependencies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentJobDependencyPolicy {
    /// Run once every dependency succeeded; otherwise `dependency_failed`.
    #[default]
    AllSucceeded,
    /// Run once every dependency is terminal, whatever its status.
    AllTerminal,
}

impl EnvironmentJobDependencyPolicy {
    pub fn is_default(&self) -> bool {
        *self == Self::AllSucceeded
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartActivityRequest {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub job_group_id: String,
    /// CAS reference of the encoded [`EnvironmentJobStartPayloadV1`].
    pub request_ref: BlobRef,
}

/// The started jobs, one record per payload job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartActivityResult {
    pub jobs: Vec<EnvironmentJobRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobSubscription {
    pub holder_workflow_id: String,
    pub promise_id: String,
    pub completion_key: String,
    pub job_id: EnvironmentJobId,
    #[serde(default)]
    pub notified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowArgs {
    pub universe_id: Uuid,
    pub start: EnvironmentJobStartActivityRequest,
    pub job_ids: Vec<EnvironmentJobId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EnvironmentJobSubscription>,
    #[serde(default)]
    pub started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<EnvironmentJobRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, PromiseSourceCheckResult>,
    #[serde(default = "default_environment_job_poll_ms")]
    pub poll_ms: u64,
    #[serde(default)]
    pub poll_attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tool: Option<EnvironmentJobWorkflowToolContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowToolContext {
    pub execution_id: String,
    pub invocation_id: harness::WorkflowToolInvocationId,
}

/// Accepted inputs for the core job workflow. Bare public starts arrive as
/// fully prepared job arguments; internally supervised starts use the fixed
/// internally supervised input and are normalized by an activity before
/// provider work begins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvironmentJobWorkflowInput {
    WorkflowTool(WorkflowToolStartArgs),
    Job(EnvironmentJobWorkflowArgs),
}

impl From<EnvironmentJobWorkflowArgs> for EnvironmentJobWorkflowInput {
    fn from(value: EnvironmentJobWorkflowArgs) -> Self {
        Self::Job(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPrepareWorkflowToolRequest {
    pub start: WorkflowToolStartArgs,
}

fn default_environment_job_poll_ms() -> u64 {
    2_000
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowSnapshot {
    pub environment_id: String,
    pub job_group_id: String,
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub jobs: Vec<EnvironmentJobRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, PromiseSourceCheckResult>,
    /// Every job of the group has a terminal record and a recorded
    /// resolution.
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Poll of the group's jobs that are not settled yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPollActivityRequest {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub job_group_id: String,
    pub job_ids: Vec<EnvironmentJobId>,
}

/// One observation per polled job. `resolutions` is keyed by job id and
/// holds the promise result of every job observed terminal, `NotFound`
/// included (a `Failed` resolution carrying the model-facing result); the
/// workflow falls back to a `Failed` resolution without a payload for a
/// `NotFound` job that has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPollActivityResult {
    pub jobs: Vec<EnvironmentJobObservation>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, PromiseSourceCheckResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelSignal {
    pub jobs: Vec<EnvironmentJobId>,
    #[serde(default)]
    pub scope: EnvironmentJobCancelScope,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelActivityRequest {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub jobs: Vec<EnvironmentJobId>,
    pub scope: EnvironmentJobCancelScope,
    pub force: bool,
}

/// One observation per job the cancel listed (named jobs first, then
/// dependents). Observations of jobs outside the group are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelActivityResult {
    pub jobs: Vec<EnvironmentJobObservation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_payload_round_trips_and_checks_its_version() {
        let payload = EnvironmentJobStartPayloadV1::new(
            "jobreq:1",
            vec![EnvironmentJobSpec {
                job_id: EnvironmentJobId::new("build"),
                name: Some("build".to_owned()),
                command: EnvironmentJobCommand {
                    argv: vec!["make".to_owned()],
                    cwd: Some("/workspace".to_owned()),
                    env: BTreeMap::new(),
                    stdin: None,
                    timeout_ms: Some(60_000),
                },
                depends_on: vec![EnvironmentJobDependency::Name {
                    name: "fetch".to_owned(),
                }],
                dependency_policy: EnvironmentJobDependencyPolicy::AllTerminal,
                queue_key: None,
            }],
        );
        let bytes = payload.encode().expect("encode payload");
        assert_eq!(
            EnvironmentJobStartPayloadV1::decode(&bytes).expect("decode payload"),
            payload
        );

        let mut future: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        future["version"] = serde_json::json!(2);
        future["extra"] = serde_json::json!(true);
        assert!(matches!(
            EnvironmentJobStartPayloadV1::decode(&serde_json::to_vec(&future).expect("encode")),
            Err(EnvironmentJobStartPayloadError::UnsupportedVersion { version: 2 })
        ));
    }

    #[test]
    fn only_live_statuses_are_not_terminal() {
        use EnvironmentJobStatus::*;
        for status in [Queued, Running] {
            assert!(!status.is_terminal());
        }
        for status in [
            Succeeded,
            Failed,
            TimedOut,
            Cancelled,
            DependencyFailed,
            Interrupted,
            Lost,
        ] {
            assert!(status.is_terminal());
        }
        assert!(Succeeded.is_success());
        assert!(!Lost.is_success());
    }
}
