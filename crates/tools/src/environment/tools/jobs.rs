//! Canonical durable-job operations.

use environment_protocol::data::jobs::{ReadJobsParams, StartJobsParams, StartJobsResponse};

use crate::{
    environment::{
        EnvironmentToolContext,
        jobs::{
            JobError, JobHandle, JobReadArgs, JobSubmitArgs, JobSubmitResult, JobSubmitted,
            ModelJobResultSet, NormalizeJobResultInput, normalize_job_result,
            visible_job_read_output,
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
    let response = jobs
        .read_jobs(ReadJobsParams {
            namespace: job_namespace(ctx, None)?,
            jobs: args.jobs.into_iter().map(|handle| handle.job_id).collect(),
            after_seq: args.after_seq,
            max_bytes: args.output_bytes,
            include_artifacts: args.include_artifacts,
            wait_ms: None,
        })
        .await?;
    let mut normalized = Vec::with_capacity(response.jobs.len());
    for job in response.jobs {
        normalized.push(
            normalize_job_result(
                ctx.blobs.as_ref(),
                NormalizeJobResultInput {
                    summary: Some(job.summary),
                    output_chunks: job.output_chunks,
                    output_next_seq: job.output_next_seq,
                    artifacts: job.artifacts,
                    output_bytes: args.output_bytes,
                    ..Default::default()
                },
            )
            .await?,
        );
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
    let mut specs = Vec::with_capacity(args.jobs.len());
    for spec in args.jobs {
        let job_id = spec.job_id.clone();
        specs.push(spec.into_protocol_spec(job_id)?);
    }
    Ok(StartJobsParams {
        namespace: job_namespace(ctx, None)?,
        request_id: "default".to_owned(),
        jobs: specs,
    })
}

fn job_namespace(
    ctx: &EnvironmentToolContext,
    explicit_session_id: Option<&str>,
) -> ToolResult<String> {
    explicit_session_id
        .map(ToOwned::to_owned)
        .or_else(|| ctx.session_id.clone())
        .ok_or_else(|| JobError::InvalidRequest {
            message: "environment job namespace requires a session_id".to_owned(),
        })
        .map_err(Into::into)
}

fn submit_result_from_response(
    ctx: &EnvironmentToolContext,
    response: StartJobsResponse,
) -> JobSubmitResult {
    JobSubmitResult {
        jobs: response
            .jobs
            .into_iter()
            .map(|summary| JobSubmitted {
                name: summary.name,
                handle: ctx.environment_id.clone().map(|environment_id| JobHandle {
                    environment_id: crate::environment::handles::environment_handle(
                        &environment_id,
                    ),
                    job_id: summary.job_id.clone(),
                }),
                job_id: summary.job_id,
                status: summary.status,
                dependencies: summary.dependencies,
                queue_key: summary.queue_key,
                promise: None,
            })
            .collect(),
    }
}
