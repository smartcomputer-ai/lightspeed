//! Durable environment job capability boundary.

use std::collections::BTreeMap;

use async_trait::async_trait;
use environment_protocol::{
    data::jobs::{
        CancelJobsParams, CancelJobsResponse, JobArtifact, JobCancelScope,
        JobDependency as ProtocolJobDependency, JobDependencyPolicy, JobOutputChunk, JobStartSpec,
        JobStatus, JobSummary, ReadJobsParams, ReadJobsResponse, StartJobsParams,
        StartJobsResponse,
    },
    shared::{ByteChunk, EnvironmentPath, JobId},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use engine::{
    BlobRef,
    storage::{BlobEdge, BlobGraphStore, BlobStore, BlobStoreError},
};

use crate::fs::FsPath;

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

pub type JobExecResult<T> = Result<T, JobError>;

#[async_trait]
pub trait JobExecutor: Send + Sync {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandle {
    pub environment_id: String,
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
    pub fn into_protocol_spec(self, job_id: JobId) -> JobExecResult<JobStartSpec> {
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
            dependency_policy: JobDependencyPolicy::AllSucceeded,
            queue_key: self.queue_key,
        }
        .into_protocol_spec(job_id)
    }
}

/// Runtime-owned facts pinned when a durable `job_submit` call is accepted.
/// The receiving workflow reads this through the generic invocation's opaque
/// execution-context reference; it is not part of the model-facing schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSubmitExecutionContextV1 {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    pub environment_id: String,
    pub allowed_provider_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_registration_key_ids: Option<Vec<String>>,
}

impl JobSubmitExecutionContextV1 {
    pub const VERSION: u32 = 1;

    pub fn new(
        environment_id: String,
        allowed_provider_ids: Option<Vec<String>>,
        allowed_registration_key_ids: Option<Vec<String>>,
    ) -> Self {
        Self {
            version: Self::VERSION,
            working_directory: None,
            environment_id,
            allowed_provider_ids,
            allowed_registration_key_ids,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDependencyArg {
    #[serde(default, alias = "jobId", skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl JobDependencyArg {
    fn into_protocol_dependency(self) -> JobExecResult<ProtocolJobDependency> {
        match (self.job_id, self.name) {
            (Some(job_id), None) => Ok(ProtocolJobDependency {
                job_id: Some(job_id),
                name: None,
            }),
            (None, Some(name)) if !name.is_empty() => Ok(ProtocolJobDependency {
                job_id: None,
                name: Some(name),
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
    pub dependency_policy: JobDependencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

impl JobSubmitSpecArgs {
    pub fn into_protocol_spec(self, job_id: JobId) -> JobExecResult<JobStartSpec> {
        if self.argv.is_empty() {
            return Err(JobError::InvalidRequest {
                message: "job argv must not be empty".to_owned(),
            });
        }
        // The model's job id keys the job's promise, so it must fit the
        // completion-key shape; checking here keeps key derivation from
        // ever failing on an accepted job.
        engine::validate_completion_key(job_id.as_str()).map_err(|error| {
            JobError::InvalidRequest {
                message: format!("job_id {:?} is invalid: {error}", job_id.as_str()),
            }
        })?;
        let depends_on = self
            .depends_on
            .into_iter()
            .map(JobDependencyArg::into_protocol_dependency)
            .collect::<JobExecResult<Vec<_>>>()?;
        Ok(JobStartSpec {
            job_id,
            name: self.name,
            argv: self.argv,
            cwd: self.cwd.as_ref().map(environment_path).transpose()?,
            env: self.env,
            secret_env: BTreeMap::new(),
            stdin: self.stdin.map(|value| ByteChunk::from(value.into_bytes())),
            timeout_ms: self.timeout_ms,
            depends_on,
            dependency_policy: self.dependency_policy,
            queue_key: self.queue_key,
        })
    }
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
pub struct JobSubmitResult {
    pub jobs: Vec<JobSubmitted>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSubmitted {
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
#[serde(rename_all = "camelCase")]
pub struct ModelJobResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<JobHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<JobSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<ModelJobOutputSegment>,
    pub output_next_seq: u64,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<JobArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelJobResultSet {
    pub jobs: Vec<ModelJobResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelJobOutputSegment {
    pub stream: environment_protocol::data::jobs::JobOutputStream,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<usize>,
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

pub fn visible_job_read_output(jobs: &[ModelJobResult]) -> String {
    serde_json::to_string_pretty(&ModelJobResultSet {
        jobs: jobs.to_vec(),
    })
    .unwrap_or_else(|error| format!("failed to encode job results: {error}"))
}

#[derive(Clone, Debug, Default)]
pub struct NormalizeJobResultInput {
    pub handle: Option<JobHandle>,
    pub summary: Option<JobSummary>,
    pub output_chunks: Vec<JobOutputChunk>,
    pub output_next_seq: u64,
    pub artifacts: Vec<JobArtifact>,
    pub error: Option<String>,
    pub output_bytes: Option<usize>,
}

pub async fn normalize_job_result(
    blobs: &dyn BlobStore,
    input: NormalizeJobResultInput,
) -> Result<ModelJobResult, BlobStoreError> {
    let NormalizeJobResultInput {
        handle,
        summary,
        output_chunks,
        output_next_seq,
        artifacts,
        error,
        output_bytes,
    } = input;
    let observed_bytes = output_chunks
        .iter()
        .map(|chunk| chunk.chunk.as_slice().len())
        .sum::<usize>();
    let truncated = output_bytes.is_some_and(|limit| observed_bytes >= limit);
    let mut output = Vec::new();
    let mut index = 0;
    while index < output_chunks.len() {
        let stream = output_chunks[index].stream;
        let mut bytes = Vec::new();
        while index < output_chunks.len() && output_chunks[index].stream == stream {
            bytes.extend_from_slice(output_chunks[index].chunk.as_slice());
            index += 1;
        }
        match String::from_utf8(bytes) {
            Ok(text) => {
                if !text.is_empty() {
                    if let Some(ModelJobOutputSegment {
                        stream: previous_stream,
                        text: Some(previous),
                        ..
                    }) = output.last_mut()
                        && *previous_stream == stream
                    {
                        previous.push_str(&text);
                    } else {
                        output.push(ModelJobOutputSegment {
                            stream,
                            text: Some(text),
                            blob_ref: None,
                            media_type: None,
                            byte_len: None,
                        });
                    }
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
    Ok(ModelJobResult {
        handle,
        summary,
        output,
        output_next_seq,
        truncated,
        artifacts,
        error,
    })
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

fn environment_path(path: &FsPath) -> JobExecResult<EnvironmentPath> {
    EnvironmentPath::new(path.as_str()).map_err(|error| JobError::InvalidRequest {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use engine::storage::{BlobEdge, BlobGraphStore, BlobStore, BlobStoreError, InMemoryBlobStore};
    use environment_protocol::data::jobs::{JobOutputStream, JobStatus};
    use serde_json::json;

    use super::*;

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
            .into_protocol_spec(JobId::new("job-derived"))
            .expect("materialize job_run");

        assert_eq!(spec.job_id, JobId::new("job-derived"));
        assert_eq!(spec.argv, vec!["cargo", "test"]);
        assert_eq!(spec.timeout_ms, Some(JOB_RUN_DEFAULT_TIMEOUT_MS));
        assert!(spec.depends_on.is_empty());
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
                "depends_on": [{"job_id": "build"}]
            }]
        }))
        .expect("decode job_submit with snake_case dependency id");

        let specs = args
            .jobs
            .into_iter()
            .map(|spec| {
                let job_id = spec.job_id.clone();
                spec.into_protocol_spec(job_id)
            })
            .collect::<JobExecResult<Vec<_>>>()
            .expect("materialize protocol specs");

        assert_eq!(
            specs[1].depends_on,
            vec![ProtocolJobDependency::job_id("build")]
        );
    }

    #[test]
    fn job_submit_accepts_camel_case_dependency_alias_for_compatibility() {
        let args = serde_json::from_value::<JobSubmitArgs>(json!({
            "jobs": [{
                "job_id": "build",
                "argv": ["true"]
            }, {
                "job_id": "test",
                "argv": ["true"],
                "depends_on": [{"jobId": "build"}]
            }]
        }))
        .expect("decode job_submit with camelCase dependency id alias");

        let spec = args.jobs.into_iter().nth(1).expect("dependent job");
        let job_id = spec.job_id.clone();
        let spec = spec
            .into_protocol_spec(job_id)
            .expect("materialize protocol spec");

        assert_eq!(
            spec.depends_on,
            vec![ProtocolJobDependency::job_id("build")]
        );
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
            .into_protocol_spec(job_id)
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
            .into_protocol_spec(JobId::new("job-derived"))
            .expect_err("timeout above maximum must fail");
        assert!(matches!(error, JobError::InvalidRequest { .. }));
    }

    #[test]
    fn job_ids_must_fit_the_completion_key_shape() {
        for job_id in ["build:1", "", "-lead", &"x".repeat(65)] {
            let error = JobSubmitSpecArgs {
                name: None,
                job_id: JobId::new(if job_id.is_empty() {
                    "placeholder"
                } else {
                    job_id
                }),
                argv: vec!["true".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: None,
                depends_on: Vec::new(),
                dependency_policy: JobDependencyPolicy::AllSucceeded,
                queue_key: None,
            }
            .into_protocol_spec(JobId::new(if job_id.is_empty() {
                "placeholder"
            } else {
                job_id
            }));
            if job_id.is_empty() {
                error.expect("placeholder id is valid");
            } else {
                assert!(
                    matches!(error, Err(JobError::InvalidRequest { .. })),
                    "{job_id:?} must be rejected"
                );
            }
        }
    }

    #[test]
    fn job_submit_execution_context_is_versioned_and_runtime_owned() {
        let context = JobSubmitExecutionContextV1::new(
            "environment-active".to_owned(),
            Some(vec!["provider-a".to_owned()]),
            None,
        );

        assert_eq!(context.version, JobSubmitExecutionContextV1::VERSION);
        assert_eq!(context.environment_id, "environment-active");
        assert_eq!(
            context.allowed_provider_ids,
            Some(vec!["provider-a".to_owned()])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalizer_reassembles_utf8_and_preserves_stream_order() {
        let blobs = InMemoryBlobStore::new();
        let result = normalize_job_result(
            &blobs,
            NormalizeJobResultInput {
                summary: Some(test_summary()),
                output_chunks: vec![
                    output_chunk(0, JobOutputStream::Stdout, vec![b'a', 0xe2]),
                    output_chunk(1, JobOutputStream::Stdout, vec![0x82, 0xac, b'\n']),
                    output_chunk(2, JobOutputStream::Stderr, b"warning\n".to_vec()),
                    output_chunk(3, JobOutputStream::Stdout, b"done\n".to_vec()),
                ],
                output_next_seq: 4,
                output_bytes: Some(1024),
                ..Default::default()
            },
        )
        .await
        .expect("normalize text");

        assert_eq!(result.output.len(), 3);
        assert_eq!(result.output[0].text.as_deref(), Some("a€\n"));
        assert_eq!(result.output[1].stream, JobOutputStream::Stderr);
        assert_eq!(result.output[2].text.as_deref(), Some("done\n"));
        assert!(!result.truncated);
        let value = serde_json::to_value(&result).expect("semantic JSON");
        assert!(value.get("outputChunks").is_none());
        assert_eq!(value["outputNextSeq"], 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalizer_stores_binary_output_as_a_ref() {
        let blobs = InMemoryBlobStore::new();
        let result = normalize_job_result(
            &blobs,
            NormalizeJobResultInput {
                summary: Some(test_summary()),
                output_chunks: vec![output_chunk(0, JobOutputStream::Stdout, vec![0xff, 0x00])],
                output_next_seq: 1,
                output_bytes: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("normalize binary");

        assert!(result.truncated);
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
                output_chunks: vec![output_chunk(0, JobOutputStream::Stdout, vec![0xff])],
                output_next_seq: 1,
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

    fn output_chunk(seq: u64, stream: JobOutputStream, bytes: Vec<u8>) -> JobOutputChunk {
        JobOutputChunk {
            seq,
            stream,
            chunk: ByteChunk::from(bytes),
        }
    }

    fn test_summary() -> JobSummary {
        JobSummary {
            namespace: "environment-a".to_owned(),
            job_id: JobId::new("job-a"),
            name: None,
            status: JobStatus::Succeeded,
            dependencies: Vec::new(),
            created_at_ms: 1,
            queued_at_ms: Some(1),
            started_at_ms: Some(2),
            finished_at_ms: Some(3),
            exit_code: Some(0),
            orphaned_descendants: false,
            failure: None,
            queue_key: None,
        }
    }
}
