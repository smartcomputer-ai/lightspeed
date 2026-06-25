//! Durable job method payloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::shared::{ByteChunk, HostPath, JobId, SecretString};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartJobsParams {
    pub namespace: String,
    pub request_id: String,
    pub jobs: Vec<JobStartSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStartSpec {
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<HostPath>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, SecretString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<ByteChunk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<JobDependency>,
    #[serde(default)]
    pub dependency_policy: JobDependencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobDependency {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl JobDependency {
    pub fn job_id(job_id: impl Into<JobId>) -> Self {
        Self {
            job_id: Some(job_id.into()),
            name: None,
        }
    }

    pub fn name(name: impl Into<String>) -> Self {
        Self {
            job_id: None,
            name: Some(name.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobDependencyPolicy {
    #[default]
    AllSucceeded,
    AllTerminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartJobsResponse {
    pub jobs: Vec<JobSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListJobsParams {
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListJobsResponse {
    pub jobs: Vec<JobSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadJobsParams {
    pub namespace: String,
    pub jobs: Vec<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub include_artifacts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadJobsResponse {
    pub jobs: Vec<JobReadResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobReadResult {
    pub summary: JobSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_chunks: Vec<JobOutputChunk>,
    pub output_next_seq: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelJobsParams {
    pub namespace: String,
    pub jobs: Vec<JobId>,
    #[serde(default)]
    pub scope: JobCancelScope,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelJobsResponse {
    pub jobs: Vec<JobSummary>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobCancelScope {
    #[default]
    Job,
    Dependents,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub namespace: String,
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: JobStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<JobId>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    Accepted,
    Queued,
    Running,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
    TimedOut,
    DependencyFailed,
    Interrupted,
    Lost,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::DependencyFailed
                | Self::Interrupted
                | Self::Lost
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutputChunk {
    pub seq: u64,
    pub stream: JobOutputStream,
    pub chunk: ByteChunk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobOutputStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobArtifact {
    pub path: HostPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}
