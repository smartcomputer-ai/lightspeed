//! Canonical durable-job operations.

use host_protocol::data::jobs::{
    CancelJobsParams, ListJobsParams, ListJobsResponse, ReadJobsParams, StartJobsParams,
    StartJobsResponse,
};

use crate::{
    environment::{
        EnvironmentToolContext,
        jobs::{
            JobCancelArgs, JobCancelResultEntry, JobCancelResultSet, JobError, JobListArgs,
            JobListResultEntry, JobListResultSet, JobReadArgs, JobReadResultEntry,
            JobReadResultSet, JobStartArgs, JobStartResult, JobStarted, JobWaitArgs,
            JobWaitOutcome, JobWaitResult, visible_job_list_output, visible_job_read_output,
            wait_satisfied,
        },
    },
    error::ToolResult,
};

use super::{invalid_request, unsupported_job_capability};

pub async fn invoke_job_start(
    ctx: &EnvironmentToolContext,
    args: JobStartArgs,
) -> ToolResult<JobStartResult> {
    if args.jobs.is_empty() {
        return Err(invalid_request("job_start requires at least one job"));
    }
    let jobs = ctx.jobs.as_ref().ok_or_else(unsupported_job_capability)?;
    let params = start_params_from_args(ctx, args)?;
    let response = jobs.start_jobs(params).await?;
    Ok(start_result_from_response(response))
}

pub async fn invoke_job_list(
    ctx: &EnvironmentToolContext,
    args: JobListArgs,
) -> ToolResult<JobListResultSet> {
    let jobs = ctx.jobs.as_ref().ok_or_else(unsupported_job_capability)?;
    let response = jobs
        .list_jobs(ListJobsParams {
            namespace: job_namespace(ctx, args.session_id.as_deref())?,
            limit: args.limit,
        })
        .await?;
    Ok(list_result_from_response(response))
}

pub async fn invoke_job_read(
    ctx: &EnvironmentToolContext,
    args: JobReadArgs,
) -> ToolResult<JobReadResultSet> {
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
    Ok(JobReadResultSet {
        jobs: response
            .jobs
            .into_iter()
            .map(|job| JobReadResultEntry {
                handle: None,
                summary: Some(job.summary),
                output_chunks: job.output_chunks,
                output_next_seq: job.output_next_seq,
                artifacts: job.artifacts,
                error: None,
            })
            .collect(),
    })
}

pub async fn invoke_job_wait(
    ctx: &EnvironmentToolContext,
    args: JobWaitArgs,
) -> ToolResult<JobWaitResult> {
    if args.jobs.is_empty() {
        return Err(invalid_request("job_wait requires at least one job"));
    }
    let jobs = ctx.jobs.as_ref().ok_or_else(unsupported_job_capability)?;
    let response = jobs
        .read_jobs(ReadJobsParams {
            namespace: job_namespace(ctx, None)?,
            jobs: args.jobs.into_iter().map(|handle| handle.job_id).collect(),
            after_seq: None,
            max_bytes: args.output_bytes,
            include_artifacts: args.include_artifacts,
            wait_ms: None,
        })
        .await?;
    let entries = response
        .jobs
        .into_iter()
        .map(|job| JobReadResultEntry {
            handle: None,
            summary: Some(job.summary),
            output_chunks: job.output_chunks,
            output_next_seq: job.output_next_seq,
            artifacts: job.artifacts,
            error: None,
        })
        .collect::<Vec<_>>();
    let outcome = if wait_satisfied(&entries, args.mode, args.terminal_policy) {
        JobWaitOutcome::Satisfied
    } else {
        JobWaitOutcome::Pending
    };
    Ok(JobWaitResult {
        outcome,
        jobs: entries,
    })
}

pub async fn invoke_job_cancel(
    ctx: &EnvironmentToolContext,
    args: JobCancelArgs,
) -> ToolResult<JobCancelResultSet> {
    if args.jobs.is_empty() {
        return Err(invalid_request("job_cancel requires at least one job"));
    }
    let jobs = ctx.jobs.as_ref().ok_or_else(unsupported_job_capability)?;
    let response = jobs
        .cancel_jobs(CancelJobsParams {
            namespace: job_namespace(ctx, None)?,
            jobs: args.jobs.into_iter().map(|handle| handle.job_id).collect(),
            scope: args.scope,
            force: args.force,
        })
        .await?;
    Ok(JobCancelResultSet {
        jobs: response
            .jobs
            .into_iter()
            .map(|summary| JobCancelResultEntry {
                handle: None,
                summary: Some(summary),
                error: None,
            })
            .collect(),
    })
}

pub fn job_read_visible(result: &JobReadResultSet) -> String {
    visible_job_read_output(&result.jobs)
}

pub fn job_list_visible(result: &JobListResultSet) -> String {
    visible_job_list_output(&result.jobs)
}

pub fn job_wait_visible(result: &JobWaitResult) -> String {
    let mut visible = format!("job_wait outcome: {:?}", result.outcome);
    let jobs = visible_job_read_output(&result.jobs);
    if !jobs.is_empty() {
        visible.push('\n');
        visible.push_str(&jobs);
    }
    visible
}

fn start_params_from_args(
    ctx: &EnvironmentToolContext,
    args: JobStartArgs,
) -> ToolResult<StartJobsParams> {
    let mut specs = Vec::with_capacity(args.jobs.len());
    for (index, spec) in args.jobs.into_iter().enumerate() {
        let Some(job_id) = spec.job_id.clone() else {
            return Err(JobError::InvalidRequest {
                message: format!(
                    "job_start jobs[{index}].job_id is required unless the runtime assigns one"
                ),
            }
            .into());
        };
        specs.push(spec.into_host_spec(job_id)?);
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

fn start_result_from_response(response: StartJobsResponse) -> JobStartResult {
    JobStartResult {
        jobs: response
            .jobs
            .into_iter()
            .map(|summary| JobStarted {
                name: summary.name,
                job_id: summary.job_id,
                handle: None,
                status: summary.status,
                dependencies: summary.dependencies,
                queue_key: summary.queue_key,
            })
            .collect(),
    }
}

fn list_result_from_response(response: ListJobsResponse) -> JobListResultSet {
    JobListResultSet {
        jobs: response
            .jobs
            .into_iter()
            .map(|summary| JobListResultEntry {
                handle: None,
                summary: Some(summary),
                error: None,
            })
            .collect(),
    }
}
