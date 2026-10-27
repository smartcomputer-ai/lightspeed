//! Canonical durable-job operations.

use environment_protocol::{
    data::job::{JobReadEntry, StartJobsParams, StartJobsResponse},
    ids::JobRequestId,
};

use crate::{
    environment::{
        EnvironmentToolContext,
        jobs::{
            DEFAULT_JOB_OUTPUT_BYTES, JobHandle, JobReadArgs, JobSubmitArgs, JobSubmitResult,
            JobSubmitted, ModelJobResultSet, NormalizeJobResultInput, job_output_read,
            normalize_job_result, visible_job_read_output,
        },
    },
    error::ToolResult,
};

use super::{invalid_request, unsupported_job_capability};

pub async fn invoke_job_submit(
    ctx: &EnvironmentToolContext,
    args: JobSubmitArgs,
) -> ToolResult<JobSubmitResult> {
    if args.jobs.is_empty() {
        return Err(invalid_request("job_submit requires at least one job"));
    }
    let jobs = ctx.jobs.as_ref().ok_or_else(unsupported_job_capability)?;
    let params = submit_params_from_args(ctx, args)?;
    let response = jobs.start_jobs(params).await?;
    Ok(submit_result_from_response(ctx, response))
}

pub async fn invoke_job_read(
    ctx: &EnvironmentToolContext,
    args: JobReadArgs,
) -> ToolResult<ModelJobResultSet> {
    if args.jobs.is_empty() {
        return Err(invalid_request("job_read requires at least one job"));
    }
    let jobs = ctx.jobs.as_ref().ok_or_else(unsupported_job_capability)?;
    let job_ids = args
        .jobs
        .iter()
        .map(|handle| handle.job_id.clone())
        .collect::<Vec<_>>();
    let mut output_bytes = args
        .output_bytes
        .unwrap_or(DEFAULT_JOB_OUTPUT_BYTES)
        .min(ctx.limits.max_model_visible_output_bytes);
    // Without `after` every job takes two `job/read` entries (head and
    // tail); the environment's `maxRequestJobs` bounds entries, not jobs,
    // so the jobs go out in as many requests as that bound needs.
    let mut after = args.after;
    let mut jobs_per_request = job_ids.len();
    if let Some(limits) = jobs.limits() {
        let max_entries = limits.max_request_jobs as usize;
        if job_ids.len() > max_entries {
            return Err(invalid_request(format!(
                "job_read names {} jobs; the environment reads at most {} at once",
                job_ids.len(),
                limits.max_request_jobs
            )));
        }
        if after.is_none() && max_entries < 2 {
            // No room for a tail entry: show the head only.
            after = Some(0);
        }
        let per_job = if after.is_some() { 1 } else { 2 };
        jobs_per_request = (max_entries / per_job).max(1);
        let in_one_request = jobs_per_request.min(job_ids.len()) as u64;
        output_bytes = output_bytes.min(limits.max_read_bytes / in_one_request);
    }
    let per_job = if after.is_some() { 1 } else { 2 };
    let mut normalized = Vec::with_capacity(job_ids.len());
    for chunk in job_ids.chunks(jobs_per_request.max(1)) {
        let response = jobs
            .read_jobs(job_output_read(chunk, after, output_bytes))
            .await?;
        let mut entries = response.jobs.into_iter();
        for _ in chunk {
            let job_entries: Vec<JobReadEntry> = entries.by_ref().take(per_job).collect();
            normalized.push(
                normalize_job_result(
                    ctx.blobs.as_ref(),
                    NormalizeJobResultInput::from_entries(None, job_entries),
                )
                .await?,
            );
        }
    }
    Ok(ModelJobResultSet { jobs: normalized })
}

pub fn job_read_visible(result: &ModelJobResultSet) -> String {
    visible_job_read_output(&result.jobs)
}

fn submit_params_from_args(
    ctx: &EnvironmentToolContext,
    args: JobSubmitArgs,
) -> ToolResult<StartJobsParams> {
    let working_directory = ctx.process_cwd.as_ref().map(|cwd| cwd.as_str());
    let mut specs = Vec::with_capacity(args.jobs.len());
    for spec in args.jobs {
        let job_id = spec.job_id.clone();
        specs.push(spec.into_job_spec(job_id, working_directory)?);
    }
    Ok(StartJobsParams {
        request_id: request_id(ctx),
        jobs: specs,
    })
}

/// One request id per call: the invocation's stable operation identity, so
/// a retried call replays its jobs while a new call that reuses a job id is
/// refused instead of answered with an earlier run.
fn request_id(ctx: &EnvironmentToolContext) -> JobRequestId {
    ctx.operation_id
        .as_deref()
        .and_then(|operation_id| JobRequestId::new(operation_id).ok())
        .unwrap_or_else(|| {
            JobRequestId::new(format!("jobreq-{}", uuid::Uuid::new_v4().simple()))
                .expect("uuid request id fits the id grammar")
        })
}

fn submit_result_from_response(
    ctx: &EnvironmentToolContext,
    response: StartJobsResponse,
) -> JobSubmitResult {
    JobSubmitResult {
        jobs: response
            .jobs
            .iter()
            .map(|summary| {
                let handle = ctx.environment_id.clone().map(|environment_id| JobHandle {
                    environment_id: crate::environment::handles::environment_handle(
                        &environment_id,
                    ),
                    job_id: summary.job_id.clone(),
                });
                JobSubmitted::from_summary(summary, handle)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use environment_protocol::{
        data::job::{
            CancelJobsParams, CancelJobsResponse, JobStatus, JobSummary, ReadJobsParams,
            ReadJobsResponse,
        },
        ids::JobId,
    };
    use harness::storage::InMemoryBlobStore;
    use serde_json::json;

    use super::*;
    use crate::environment::jobs::{JobExecResult, JobExecutor};

    #[derive(Default)]
    struct RecordingJobs {
        starts: Mutex<Vec<StartJobsParams>>,
        reads: Mutex<Vec<ReadJobsParams>>,
        limits: Option<environment_protocol::data::handshake::JobLimits>,
    }

    #[async_trait]
    impl JobExecutor for RecordingJobs {
        fn limits(&self) -> Option<environment_protocol::data::handshake::JobLimits> {
            self.limits.clone()
        }

        async fn start_jobs(&self, request: StartJobsParams) -> JobExecResult<StartJobsResponse> {
            let jobs = request
                .jobs
                .iter()
                .map(|job| JobSummary {
                    job_id: job.job_id.clone(),
                    request_id: request.request_id.clone(),
                    name: None,
                    status: JobStatus::Queued,
                    depends_on: Vec::new(),
                    queue_key: None,
                    created_at_ms: 1,
                    queued_at_ms: None,
                    started_at_ms: None,
                    finished_at_ms: None,
                    cancel_requested_at_ms: None,
                    duration_ms: None,
                    exit: None,
                    stopped_by: None,
                    leftovers: None,
                    error: None,
                })
                .collect();
            self.starts.lock().unwrap().push(request);
            Ok(StartJobsResponse { jobs })
        }

        async fn read_jobs(&self, request: ReadJobsParams) -> JobExecResult<ReadJobsResponse> {
            let jobs = request
                .jobs
                .iter()
                .map(|entry| JobReadEntry::Error {
                    job_id: entry.job_id.clone(),
                    error: environment_protocol::error::ProtocolError::not_found("unknown job"),
                })
                .collect();
            self.reads.lock().unwrap().push(request);
            Ok(ReadJobsResponse { jobs })
        }

        async fn cancel_jobs(
            &self,
            _request: CancelJobsParams,
        ) -> JobExecResult<CancelJobsResponse> {
            unreachable!("not cancelled")
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_job_submits_send_a_request_id_per_call() {
        let jobs = Arc::new(RecordingJobs::default());
        let base = EnvironmentToolContext::new(None, Arc::new(InMemoryBlobStore::new()))
            .with_jobs(jobs.clone());
        let args = || {
            serde_json::from_value::<JobSubmitArgs>(json!({
                "jobs": [{"job_id": "build", "argv": ["make"]}]
            }))
            .unwrap()
        };
        let first = base.clone().with_operation_id("tool-first");
        invoke_job_submit(&first, args()).await.unwrap();
        invoke_job_submit(&first, args()).await.unwrap();
        let second = base.with_operation_id("tool-second");
        let submitted = invoke_job_submit(&second, args()).await.unwrap();

        let starts = jobs.starts.lock().unwrap();
        assert_eq!(starts[0].request_id.as_str(), "tool-first");
        assert_eq!(
            starts[0].request_id, starts[1].request_id,
            "a retried call replays"
        );
        assert_ne!(
            starts[0].request_id, starts[2].request_id,
            "a new call is a new run"
        );
        assert_eq!(starts[0].jobs[0].job_id, JobId::new("build").unwrap());
        assert_eq!(
            serde_json::to_value(&submitted).unwrap()["jobs"][0]["status"],
            "queued"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_reads_split_head_and_tail_entries_across_requests() {
        let jobs = Arc::new(RecordingJobs {
            limits: Some(environment_protocol::data::handshake::JobLimits {
                max_read_bytes: 4096,
                max_request_jobs: 4,
                max_retained_output_bytes: 1 << 20,
                record_retention_ms: 1,
                cancel_grace_ms: 1,
            }),
            ..RecordingJobs::default()
        });
        let ctx = EnvironmentToolContext::new(None, Arc::new(InMemoryBlobStore::new()))
            .with_jobs(jobs.clone());
        let args = serde_json::from_value::<JobReadArgs>(json!({
            "jobs": [{"job_id": "a"}, {"job_id": "b"}, {"job_id": "c"}]
        }))
        .unwrap();
        let result = invoke_job_read(&ctx, args).await.unwrap();

        assert_eq!(result.jobs.len(), 3);
        let reads = jobs.reads.lock().unwrap();
        assert_eq!(reads.len(), 2, "two jobs of head and tail fill one request");
        assert!(reads.iter().all(|read| read.jobs.len() <= 4));
        assert!(reads.iter().all(|read| read.max_bytes <= 4096));
        let named = reads
            .iter()
            .flat_map(|read| read.jobs.iter().map(|entry| entry.job_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(named, ["a", "a", "b", "b", "c", "c"]);
    }
}
