//! Activities of `EnvironmentJobWorkflow`.
//!
//! The workflow keeps its own job records and start payload in history;
//! these activities are the only place that maps them to and from the
//! environment protocol's `job/*` wire types.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use environment_client::{ClientError, DataClient};
use environment_protocol::{
    bytes::ByteChunk,
    data::{
        exec::{CommandSpec, ExecInput, ReturnOn},
        job::{
            CancelJobsParams, JobCancelScope, JobDependency, JobDependencyPolicy, JobReadEntry,
            JobReadRequest, JobSpec, JobStatus, JobSummary, JobSummaryEntry, ReadJobsParams,
            StartJobsParams,
        },
    },
    error::{ProtocolError, ProtocolErrorKind},
    ids::{JobId, JobName, JobRequestId, QueueKey},
    paths::EnvironmentPath,
};
use environments::{EnvironmentId, EnvironmentJobGroupId, EnvironmentRecord};
use harness::{BlobRef, PromiseSourceCheckResult, storage::BlobStore};
use temporal_workflow::{
    EnvironmentJobCancelActivityRequest, EnvironmentJobCancelActivityResult,
    EnvironmentJobCancelScope, EnvironmentJobCommand, EnvironmentJobDependency,
    EnvironmentJobDependencyPolicy, EnvironmentJobId, EnvironmentJobObservation,
    EnvironmentJobPollActivityRequest, EnvironmentJobPollActivityResult,
    EnvironmentJobPrepareWorkflowToolRequest, EnvironmentJobRecord, EnvironmentJobSpec,
    EnvironmentJobStartActivityRequest, EnvironmentJobStartActivityResult,
    EnvironmentJobStartPayloadV1, EnvironmentJobStatus, EnvironmentJobSubscription,
    EnvironmentJobWorkflowArgs, EnvironmentJobWorkflowToolContext,
};
use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;
use tools::environment::jobs::{
    DEFAULT_JOB_OUTPUT_BYTES, JOB_RUN_WORKFLOW_SEMANTIC_TYPE, JOB_RUN_WORKFLOW_TOOL_ID,
    JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE, JOB_SUBMIT_WORKFLOW_TOOL_ID, JobHandle,
    NormalizeJobResultInput, job_output_read, model_job_error, normalize_job_result,
    store_model_job_result,
};

use super::{common::activity_error, state::EnvironmentJobActivityDeps};

/// `User-Agent` of the job activities' worker-route upgrades; `clientInfo`
/// is always the runtime build.
const JOB_ACTIVITY_USER_AGENT: &str = "lightspeed-environment-job-workflow";

/// Deadline of every data-plane call these background activities make.
const JOB_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum WorkflowJobToolKind {
    Submit,
    Run,
}

impl WorkflowJobToolKind {
    fn name(self) -> &'static str {
        match self {
            Self::Submit => "job_submit",
            Self::Run => "job_run",
        }
    }
}

pub(super) async fn prepare_workflow_tool(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobPrepareWorkflowToolRequest,
) -> Result<EnvironmentJobWorkflowArgs, ActivityError> {
    let deps = configured(deps)?;
    let start = request.start;
    let expected_holder =
        temporal_workflow::compose_workflow_id(start.universe_id, &start.invocation.session_id);
    if start.execution_id.is_empty()
        || start.universe_id != start.invocation.session_universe_id
        || start.holder_workflow_id != expected_holder
    {
        return Err(activity_error(anyhow::anyhow!(
            "environment job workflow-tool start identity is invalid"
        )));
    }
    let tool_kind = match (
        start.invocation.tool_id.as_str(),
        start.invocation.semantic_type.as_str(),
    ) {
        (JOB_SUBMIT_WORKFLOW_TOOL_ID, JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE) => {
            WorkflowJobToolKind::Submit
        }
        (JOB_RUN_WORKFLOW_TOOL_ID, JOB_RUN_WORKFLOW_SEMANTIC_TYPE) => WorkflowJobToolKind::Run,
        _ => {
            return Err(activity_error(anyhow::anyhow!(
                "unsupported environment job workflow tool {} ({})",
                start.invocation.tool_id,
                start.invocation.semantic_type
            )));
        }
    };
    let arguments = deps
        .blobs
        .read_bytes(&start.invocation.arguments_ref)
        .await
        .map_err(activity_error)?;
    let execution_context_ref =
        start
            .invocation
            .execution_context_ref
            .as_ref()
            .ok_or_else(|| {
                activity_error(anyhow::anyhow!(
                    "{} invocation is missing its execution context",
                    tool_kind.name()
                ))
            })?;
    let execution_context = deps
        .blobs
        .read_bytes(execution_context_ref)
        .await
        .map_err(activity_error)?;
    let execution_context: tools::environment::jobs::JobSubmitExecutionContextV1 =
        serde_json::from_slice(&execution_context).map_err(activity_error)?;
    if execution_context.version != tools::environment::jobs::JobSubmitExecutionContextV1::VERSION {
        return Err(activity_error(anyhow::anyhow!(
            "unsupported job_submit execution context version {}",
            execution_context.version
        )));
    }
    let environment_id =
        EnvironmentId::try_new(execution_context.environment_id).map_err(activity_error)?;
    let working_directory = execution_context.working_directory.as_deref();
    // The executor admitted this call against the session's attachment
    // grant; the context names the machine it admitted.
    let request_id = workflow_tool_request_id(
        start.invocation.run_id.as_u64(),
        start.invocation.turn_id.as_u64(),
        start.invocation.tool_batch_id.as_u64(),
        start.invocation.tool_call_id.as_str(),
    );
    let wire_jobs = match tool_kind {
        WorkflowJobToolKind::Submit => {
            let args: tools::environment::jobs::JobSubmitArgs =
                serde_json::from_slice(&arguments).map_err(activity_error)?;
            if args.jobs.is_empty() {
                return Err(activity_error(anyhow::anyhow!(
                    "job_submit requires at least one job"
                )));
            }
            args.jobs
                .into_iter()
                .map(|spec| {
                    let job_id = spec.job_id.clone();
                    spec.into_job_spec(job_id, working_directory)
                        .map_err(|error| activity_error(anyhow::anyhow!(error.to_string())))
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        WorkflowJobToolKind::Run => {
            let args: tools::environment::jobs::JobRunArgs =
                serde_json::from_slice(&arguments).map_err(activity_error)?;
            let job_id = derived_job_run_id(start.invocation.invocation_id.as_str());
            vec![
                args.into_job_spec(job_id, working_directory)
                    .map_err(|error| activity_error(anyhow::anyhow!(error.to_string())))?,
            ]
        }
    };
    let jobs = wire_jobs
        .into_iter()
        .map(workflow_job_spec)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| activity_error(anyhow::anyhow!(error)))?;
    let payload = EnvironmentJobStartPayloadV1::new(request_id.into_string(), jobs);
    let completion_promises = start
        .invocation
        .completion_promises
        .as_ref()
        .ok_or_else(|| {
            activity_error(anyhow::anyhow!(
                "{} workflow invocation is missing completion promises",
                tool_kind.name()
            ))
        })?;
    let subscriptions = match tool_kind {
        WorkflowJobToolKind::Submit => {
            if completion_promises.len() != payload.jobs.len() {
                return Err(activity_error(anyhow::anyhow!(
                    "job_submit completion promise count does not match job count"
                )));
            }
            payload
                .jobs
                .iter()
                .map(|job| {
                    // The model's job id is the completion key (the
                    // binding's `ArrayItemField` source), so the promise
                    // map it received is keyed by the names it chose.
                    let completion_key = job.job_id.as_str().to_owned();
                    let promise_id = completion_promises.get(&completion_key).ok_or_else(|| {
                        activity_error(anyhow::anyhow!(
                            "job_submit is missing completion key {completion_key}"
                        ))
                    })?;
                    Ok(EnvironmentJobSubscription {
                        holder_workflow_id: start.holder_workflow_id.clone(),
                        promise_id: promise_id.as_str().to_owned(),
                        completion_key,
                        job_id: job.job_id.clone(),
                        notified: false,
                    })
                })
                .collect::<Result<Vec<_>, ActivityError>>()?
        }
        WorkflowJobToolKind::Run => {
            if completion_promises.len() != 1 {
                return Err(activity_error(anyhow::anyhow!(
                    "job_run requires exactly one completion promise"
                )));
            }
            let promise_id = completion_promises
                .get(harness::REPLY_COMPLETION_KEY)
                .ok_or_else(|| {
                    activity_error(anyhow::anyhow!(
                        "job_run is missing completion key {}",
                        harness::REPLY_COMPLETION_KEY
                    ))
                })?;
            vec![EnvironmentJobSubscription {
                holder_workflow_id: start.holder_workflow_id.clone(),
                promise_id: promise_id.as_str().to_owned(),
                completion_key: harness::REPLY_COMPLETION_KEY.to_owned(),
                job_id: payload.jobs[0].job_id.clone(),
                notified: false,
            }]
        }
    };
    let encoded = payload.encode().map_err(activity_error)?;
    let job_group_id = derived_job_group_id(&environment_id, &BlobRef::from_bytes(&encoded));
    let request_ref = deps
        .blobs
        .put_bytes(encoded)
        .await
        .map_err(activity_error)?;
    Ok(EnvironmentJobWorkflowArgs {
        universe_id: start.universe_id,
        start: EnvironmentJobStartActivityRequest {
            universe_id: start.universe_id,
            environment_id: environment_id.as_str().to_owned(),
            job_group_id: job_group_id.as_str().to_owned(),
            request_ref,
        },
        job_ids: payload.job_ids(),
        subscriptions,
        started: false,
        jobs: Vec::new(),
        resolutions: BTreeMap::new(),
        poll_ms: 2_000,
        poll_attempt: 0,
        workflow_tool: Some(EnvironmentJobWorkflowToolContext {
            execution_id: start.execution_id,
            invocation_id: start.invocation.invocation_id,
        }),
    })
}

/// The start's idempotency key: one per model tool call, so a retried call
/// replays its jobs and a new call reusing a job id is refused.
fn workflow_tool_request_id(run: u64, turn: u64, batch: u64, call_id: &str) -> JobRequestId {
    JobRequestId::new(format!("jobreq:{run}:{turn}:{batch}:{call_id}")).unwrap_or_else(|_| {
        // A provider call id outside the id grammar is hashed instead.
        let hash = BlobRef::from_bytes(format!("{run}:{turn}:{batch}:{call_id}").as_bytes());
        JobRequestId::new(format!("jobreq-{}", &hash.as_str()[7..39]))
            .expect("hex request id fits the id grammar")
    })
}

fn derived_job_run_id(invocation_id: &str) -> JobId {
    let hash = BlobRef::from_bytes(invocation_id.as_bytes());
    JobId::new(format!("job-{}", &hash.as_str()[7..31])).expect("hex job id fits the id grammar")
}

/// A group is named by its environment and the content address of its
/// encoded start payload, which is also the payload's CAS `request_ref`.
fn derived_job_group_id(
    environment_id: &EnvironmentId,
    payload_ref: &BlobRef,
) -> EnvironmentJobGroupId {
    let hash = BlobRef::from_bytes(format!("{environment_id}:{payload_ref}").as_bytes());
    EnvironmentJobGroupId::new(format!("ejg_{}", &hash.as_str()[7..31]))
}

/// The workflow's record of a wire job spec built by the model-facing
/// arguments. The spec never carries secrets here: the start activity
/// resolves credential-bound variables.
fn workflow_job_spec(spec: JobSpec) -> Result<EnvironmentJobSpec, String> {
    let JobSpec {
        job_id,
        name,
        command,
        depends_on,
        dependency_policy,
        queue_key,
    } = spec;
    let CommandSpec {
        argv,
        cwd,
        env,
        secret_env,
        input,
        timeout_ms,
    } = command;
    if !secret_env.is_empty() {
        return Err(format!(
            "job {job_id} carries secret variables before credential resolution"
        ));
    }
    let stdin = match input {
        ExecInput::None {} => None,
        ExecInput::Bytes { data } => Some(
            String::from_utf8(data.into_inner())
                .map_err(|_| format!("job {job_id} stdin is not UTF-8"))?,
        ),
        ExecInput::Pipe {} => return Err(format!("job {job_id} cannot read a pipe")),
    };
    Ok(EnvironmentJobSpec {
        job_id: EnvironmentJobId::new(job_id.into_string()),
        name: name.map(JobName::into_string),
        command: EnvironmentJobCommand {
            argv,
            cwd: cwd.map(|cwd| cwd.as_str().to_owned()),
            env,
            stdin,
            timeout_ms,
        },
        depends_on: depends_on
            .into_iter()
            .map(|dependency| match dependency {
                JobDependency::JobId { job_id } => EnvironmentJobDependency::JobId {
                    job_id: EnvironmentJobId::new(job_id.into_string()),
                },
                JobDependency::Name { name } => EnvironmentJobDependency::Name {
                    name: name.into_string(),
                },
            })
            .collect(),
        dependency_policy: match dependency_policy {
            JobDependencyPolicy::AllSucceeded => EnvironmentJobDependencyPolicy::AllSucceeded,
            JobDependencyPolicy::AllTerminal => EnvironmentJobDependencyPolicy::AllTerminal,
        },
        queue_key: queue_key.map(QueueKey::into_string),
    })
}

/// The wire spec of a stored job, without its secret variables.
fn wire_job_spec(spec: EnvironmentJobSpec) -> Result<JobSpec, String> {
    let EnvironmentJobSpec {
        job_id,
        name,
        command,
        depends_on,
        dependency_policy,
        queue_key,
    } = spec;
    let job_id = wire_job_id(&job_id)?;
    Ok(JobSpec {
        name: name
            .map(|name| JobName::new(name).map_err(|error| format!("job {job_id} name: {error}")))
            .transpose()?,
        command: CommandSpec {
            argv: command.argv,
            cwd: command
                .cwd
                .map(|cwd| {
                    EnvironmentPath::new(cwd).map_err(|error| format!("job {job_id} cwd: {error}"))
                })
                .transpose()?,
            env: command.env,
            secret_env: BTreeMap::new(),
            input: match command.stdin {
                Some(stdin) => ExecInput::Bytes {
                    data: ByteChunk::from(stdin.into_bytes()),
                },
                None => ExecInput::None {},
            },
            timeout_ms: command.timeout_ms,
        },
        depends_on: depends_on
            .into_iter()
            .map(|dependency| match dependency {
                EnvironmentJobDependency::JobId { job_id } => {
                    wire_job_id(&job_id).map(|job_id| JobDependency::JobId { job_id })
                }
                EnvironmentJobDependency::Name { name } => JobName::new(name)
                    .map(|name| JobDependency::Name { name })
                    .map_err(|error| format!("job dependency name: {error}")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        dependency_policy: match dependency_policy {
            EnvironmentJobDependencyPolicy::AllSucceeded => JobDependencyPolicy::AllSucceeded,
            EnvironmentJobDependencyPolicy::AllTerminal => JobDependencyPolicy::AllTerminal,
        },
        queue_key: queue_key
            .map(|key| {
                QueueKey::new(key).map_err(|error| format!("job {job_id} queue key: {error}"))
            })
            .transpose()?,
        job_id,
    })
}

fn wire_job_id(job_id: &EnvironmentJobId) -> Result<JobId, String> {
    JobId::new(job_id.as_str()).map_err(|error| format!("job id {job_id}: {error}"))
}

/// The workflow's record of a daemon summary.
fn job_record(summary: &JobSummary) -> EnvironmentJobRecord {
    EnvironmentJobRecord {
        job_id: EnvironmentJobId::new(summary.job_id.as_str()),
        name: summary.name.as_ref().map(|name| name.as_str().to_owned()),
        status: job_status(summary.status),
        depends_on: summary
            .depends_on
            .iter()
            .map(|job_id| EnvironmentJobId::new(job_id.as_str()))
            .collect(),
        queue_key: summary
            .queue_key
            .as_ref()
            .map(|key| key.as_str().to_owned()),
    }
}

fn job_status(status: JobStatus) -> EnvironmentJobStatus {
    match status {
        JobStatus::Queued => EnvironmentJobStatus::Queued,
        JobStatus::Running => EnvironmentJobStatus::Running,
        JobStatus::Succeeded => EnvironmentJobStatus::Succeeded,
        JobStatus::Failed => EnvironmentJobStatus::Failed,
        JobStatus::TimedOut => EnvironmentJobStatus::TimedOut,
        JobStatus::Cancelled => EnvironmentJobStatus::Cancelled,
        JobStatus::DependencyFailed => EnvironmentJobStatus::DependencyFailed,
        JobStatus::Interrupted => EnvironmentJobStatus::Interrupted,
    }
}

/// A per-entry error: `notFound` is the workflow's `lost` outcome; any
/// other entry error means the daemon broke the contract.
fn entry_error_observation(
    job_id: &JobId,
    error: ProtocolError,
) -> Result<EnvironmentJobObservation, ActivityError> {
    if error.kind() == ProtocolErrorKind::NotFound {
        Ok(EnvironmentJobObservation::NotFound {
            job_id: EnvironmentJobId::new(job_id.as_str()),
        })
    } else {
        Err(activity_error(anyhow::anyhow!(
            "environment answered job {job_id} with {error}"
        )))
    }
}

pub(super) async fn start(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobStartActivityRequest,
) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
    let deps = configured(deps)?;
    let environment_id =
        EnvironmentId::try_new(request.environment_id.clone()).map_err(activity_error)?;
    let payload = EnvironmentJobStartPayloadV1::decode(
        &deps
            .blobs
            .read_bytes(&request.request_ref)
            .await
            .map_err(activity_error)?,
    )
    .map_err(non_retryable_activity_error)?;
    let request_id = JobRequestId::new(payload.request_id.clone()).map_err(|error| {
        non_retryable_activity_error(anyhow::anyhow!("job request id: {error}"))
    })?;
    let requested_ids = payload.job_ids().into_iter().collect::<BTreeSet<_>>();
    let mut jobs = Vec::with_capacity(payload.jobs.len());
    for spec in payload.jobs {
        let mut job = wire_job_spec(spec)
            .map_err(|error| non_retryable_activity_error(anyhow::anyhow!(error)))?;
        let secret_env = deps
            .credentials
            .resolve_secret_env(&environment_id, &job.command.env)
            .await
            .map_err(activity_error)?;
        job.command.secret_env.extend(secret_env);
        jobs.push(job);
    }

    let instance = usable_instance(deps, &environment_id).await?;
    let client = job_client(deps, &instance).await?;
    let response = client
        .start_jobs(&StartJobsParams { request_id, jobs })
        .await;
    let _ = client.close().await;
    let response = response.map_err(client_activity_error)?;
    let records = response.jobs.iter().map(job_record).collect::<Vec<_>>();
    let returned_ids = records
        .iter()
        .map(|record| record.job_id.clone())
        .collect::<BTreeSet<_>>();
    if requested_ids != returned_ids {
        return Err(non_retryable_activity_error(anyhow::anyhow!(
            "environment start response job ids do not match the request"
        )));
    }
    Ok(EnvironmentJobStartActivityResult { jobs: records })
}

pub(super) async fn poll(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobPollActivityRequest,
) -> Result<EnvironmentJobPollActivityResult, ActivityError> {
    let deps = configured(deps)?;
    let environment_id = EnvironmentId::try_new(request.environment_id).map_err(activity_error)?;
    let job_ids = request
        .job_ids
        .iter()
        .map(wire_job_id)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| non_retryable_activity_error(anyhow::anyhow!(error)))?;
    if job_ids.is_empty() {
        return Ok(EnvironmentJobPollActivityResult {
            jobs: Vec::new(),
            resolutions: BTreeMap::new(),
        });
    }
    let instance = usable_instance(deps, &environment_id).await?;
    let client = job_client(deps, &instance).await?;
    let result = poll_jobs(deps, &client, &environment_id, &job_ids).await;
    let _ = client.close().await;
    result
}

async fn poll_jobs(
    deps: &EnvironmentJobActivityDeps,
    client: &DataClient,
    environment_id: &EnvironmentId,
    job_ids: &[JobId],
) -> Result<EnvironmentJobPollActivityResult, ActivityError> {
    let limits = client.initialize_response().limits.job.clone();
    let per_request = (limits.max_request_jobs as usize).max(1);
    let mut jobs = Vec::with_capacity(job_ids.len());
    let mut terminal = Vec::new();
    // Summaries first, without output: most polls find running jobs.
    for chunk in job_ids.chunks(per_request) {
        let response = client
            .read_jobs(&ReadJobsParams {
                jobs: chunk
                    .iter()
                    .map(|job_id| JobReadRequest {
                        job_id: job_id.clone(),
                        after: None,
                        tail_bytes: None,
                        max_bytes: None,
                    })
                    .collect(),
                max_bytes: 0,
                wait_ms: 0,
                return_on: ReturnOn::AnyOutput,
            })
            .await
            .map_err(client_activity_error)?;
        for (job_id, entry) in chunk.iter().zip(response.jobs) {
            match entry {
                JobReadEntry::Job { summary, .. } => {
                    let record = job_record(&summary);
                    if record.status.is_terminal() {
                        terminal.push((job_id.clone(), record.status));
                    }
                    jobs.push(EnvironmentJobObservation::Job { record });
                }
                JobReadEntry::Error { job_id, error } => {
                    jobs.push(entry_error_observation(&job_id, error)?);
                }
            }
        }
    }

    let mut resolutions = BTreeMap::new();
    for observation in &jobs {
        if let EnvironmentJobObservation::NotFound { job_id } = observation {
            let handle = job_handle(environment_id, job_id)?;
            let result = model_job_error(
                Some(handle),
                format!("environment no longer knows job {job_id}"),
            );
            let result_ref =
                store_model_job_result(deps.blobs.as_ref(), deps.blob_graph.as_deref(), &result)
                    .await
                    .map_err(activity_error)?;
            resolutions.insert(
                job_id.as_str().to_owned(),
                PromiseSourceCheckResult::Failed {
                    error_ref: Some(result_ref),
                },
            );
        }
    }

    // Each terminal job's result shows the head and the tail of its output.
    let per_read = (per_request / 2).max(1);
    for chunk in terminal.chunks(per_read) {
        let ids = chunk
            .iter()
            .map(|(job_id, _)| job_id.clone())
            .collect::<Vec<_>>();
        let output_bytes = DEFAULT_JOB_OUTPUT_BYTES.min(limits.max_read_bytes / ids.len() as u64);
        let response = client
            .read_jobs(&job_output_read(&ids, None, output_bytes))
            .await
            .map_err(client_activity_error)?;
        let mut entries = response.jobs.into_iter();
        for (job_id, status) in chunk {
            let handle = job_handle(environment_id, &EnvironmentJobId::new(job_id.as_str()))?;
            let mut input = NormalizeJobResultInput::from_entries(
                Some(handle),
                entries.by_ref().take(2).collect::<Vec<_>>(),
            );
            if !status.is_success() && input.error.is_none() {
                input.error = Some(
                    match input.summary.as_ref().and_then(|s| s.error.as_ref()) {
                        Some(error) => {
                            format!("environment job {job_id} ended {status:?}: {error}")
                        }
                        None => format!("environment job {job_id} ended {status:?}"),
                    },
                );
            }
            let normalized = normalize_job_result(deps.blobs.as_ref(), input)
                .await
                .map_err(activity_error)?;
            let result_ref = store_model_job_result(
                deps.blobs.as_ref(),
                deps.blob_graph.as_deref(),
                &normalized,
            )
            .await
            .map_err(activity_error)?;
            let resolution = if status.is_success() {
                PromiseSourceCheckResult::Resolved {
                    payload_ref: Some(result_ref),
                }
            } else {
                PromiseSourceCheckResult::Failed {
                    error_ref: Some(result_ref),
                }
            };
            resolutions.insert(job_id.as_str().to_owned(), resolution);
        }
    }
    Ok(EnvironmentJobPollActivityResult { jobs, resolutions })
}

fn job_handle(
    environment_id: &EnvironmentId,
    job_id: &EnvironmentJobId,
) -> Result<JobHandle, ActivityError> {
    Ok(JobHandle {
        environment_id: environment_id.as_str().to_owned(),
        job_id: wire_job_id(job_id)
            .map_err(|error| non_retryable_activity_error(anyhow::anyhow!(error)))?,
    })
}

pub(super) async fn cancel(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobCancelActivityRequest,
) -> Result<EnvironmentJobCancelActivityResult, ActivityError> {
    let deps = configured(deps)?;
    let environment_id = EnvironmentId::try_new(request.environment_id).map_err(activity_error)?;
    let job_ids = request
        .jobs
        .iter()
        .map(wire_job_id)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| non_retryable_activity_error(anyhow::anyhow!(error)))?;
    if job_ids.is_empty() {
        return Ok(EnvironmentJobCancelActivityResult { jobs: Vec::new() });
    }
    let instance = usable_instance(deps, &environment_id).await?;
    let client = job_client(deps, &instance).await?;
    let result = cancel_jobs(&client, &job_ids, request.scope, request.force).await;
    let _ = client.close().await;
    result
}

async fn cancel_jobs(
    client: &DataClient,
    job_ids: &[JobId],
    scope: EnvironmentJobCancelScope,
    force: bool,
) -> Result<EnvironmentJobCancelActivityResult, ActivityError> {
    let per_request = (client.initialize_response().limits.job.max_request_jobs as usize).max(1);
    let scope = match scope {
        EnvironmentJobCancelScope::Job => JobCancelScope::Job,
        EnvironmentJobCancelScope::Dependents => JobCancelScope::Dependents,
    };
    let mut jobs = Vec::new();
    for chunk in job_ids.chunks(per_request) {
        let response = client
            .cancel_jobs(&CancelJobsParams {
                job_ids: chunk.to_vec(),
                scope,
                force,
            })
            .await
            .map_err(client_activity_error)?;
        for entry in response.jobs {
            jobs.push(match entry {
                JobSummaryEntry::Job { summary } => EnvironmentJobObservation::Job {
                    record: job_record(&summary),
                },
                JobSummaryEntry::Error { job_id, error } => {
                    entry_error_observation(&job_id, error)?
                }
            });
        }
    }
    Ok(EnvironmentJobCancelActivityResult { jobs })
}

fn configured(
    deps: Option<&EnvironmentJobActivityDeps>,
) -> Result<&EnvironmentJobActivityDeps, ActivityError> {
    deps.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })
}

/// The environment through the single use gate before a job call dials it:
/// the activity retries a wait (provisioning, or a daemon reconnecting) and
/// fails for good on an environment that is closing, closed, failed or away.
async fn usable_instance(
    deps: &EnvironmentJobActivityDeps,
    environment_id: &EnvironmentId,
) -> Result<EnvironmentRecord, ActivityError> {
    use crate::environments::resolver::{EnvironmentResolveError, EnvironmentResolver};

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(activity_error)?
        .as_millis();
    EnvironmentResolver::new(deps.environments.clone())
        .resolve_for_connection(environment_id, i64::try_from(now_ms).unwrap_or(i64::MAX))
        .await
        .map_err(|error| match error {
            EnvironmentResolveError::NotReady { .. }
            | EnvironmentResolveError::Unreachable { .. }
            | EnvironmentResolveError::Store(environments::EnvironmentRegistryError::Store {
                ..
            }) => activity_error(error),
            other => non_retryable_activity_error(other),
        })
}

/// An initialized data client for the environment; the daemon must serve
/// the `job` family.
async fn job_client(
    deps: &EnvironmentJobActivityDeps,
    instance: &EnvironmentRecord,
) -> Result<DataClient, ActivityError> {
    let gateway = deps.gateway.as_ref().ok_or_else(|| {
        non_retryable_activity_error(anyhow::anyhow!(
            "environment gateway is not configured on this worker"
        ))
    })?;
    let endpoint = gateway.endpoint_for(deps.universe_id, instance);
    let (client, initialized) = DataClient::connect_initialized(
        &endpoint,
        gateway
            .connect_options(JOB_ACTIVITY_USER_AGENT)
            .with_request_timeout(JOB_REQUEST_TIMEOUT),
    )
    .await
    .map_err(client_activity_error)?;
    if initialized.capabilities.job.is_none() {
        let _ = client.close().await;
        return Err(non_retryable_activity_error(anyhow::anyhow!(
            "environment does not serve durable jobs: {}",
            instance.environment_id
        )));
    }
    Ok(client)
}

/// Retry what may pass on a later attempt (transport trouble, busy or
/// unavailable daemons, server defects); refusals of the request itself,
/// such as `invalidParams` or `failedPrecondition {jobIdInUse}`, and
/// version or plane mismatches fail the activity for good.
fn client_activity_error(error: ClientError) -> ActivityError {
    if is_transient(&error) {
        activity_error(error)
    } else {
        non_retryable_activity_error(error)
    }
}

fn is_transient(error: &ClientError) -> bool {
    error.is_retryable()
        || matches!(
            error,
            ClientError::Decode(_) | ClientError::InvalidMessage(_)
        )
        || error.protocol_kind() == Some(ProtocolErrorKind::Internal)
}

fn non_retryable_activity_error(error: impl Into<anyhow::Error>) -> ActivityError {
    ActivityError::application(ApplicationFailure::non_retryable(error.into()))
}

#[cfg(test)]
mod tests {
    use environment_protocol::error::PreconditionReason;

    use super::*;

    fn submitted_spec() -> JobSpec {
        JobSpec {
            job_id: JobId::new("build").unwrap(),
            name: Some(JobName::new("build & test").unwrap()),
            command: CommandSpec {
                argv: vec!["make".to_owned(), "test".to_owned()],
                cwd: Some(EnvironmentPath::new("/workspace").unwrap()),
                env: BTreeMap::from([("CI".to_owned(), "1".to_owned())]),
                secret_env: BTreeMap::new(),
                input: ExecInput::Bytes {
                    data: ByteChunk::from(b"input".to_vec()),
                },
                timeout_ms: Some(1_000),
            },
            depends_on: vec![
                JobDependency::JobId {
                    job_id: JobId::new("setup").unwrap(),
                },
                JobDependency::Name {
                    name: JobName::new("lint").unwrap(),
                },
            ],
            dependency_policy: JobDependencyPolicy::AllTerminal,
            queue_key: Some(QueueKey::new("ci").unwrap()),
        }
    }

    #[test]
    fn stored_specs_map_back_onto_the_wire_unchanged() {
        let spec = submitted_spec();
        let stored = workflow_job_spec(spec.clone()).unwrap();
        assert_eq!(stored.command.stdin.as_deref(), Some("input"));
        assert_eq!(stored.command.cwd.as_deref(), Some("/workspace"));
        assert_eq!(wire_job_spec(stored).unwrap(), spec);
    }

    #[test]
    fn stored_specs_never_hold_secrets_or_pipes() {
        let mut secret = submitted_spec();
        secret
            .command
            .secret_env
            .insert("TOKEN".to_owned(), "value".into());
        assert!(workflow_job_spec(secret).is_err());
        let mut piped = submitted_spec();
        piped.command.input = ExecInput::Pipe {};
        assert!(workflow_job_spec(piped).is_err());
    }

    #[test]
    fn invalid_stored_ids_are_refused_at_the_wire_boundary() {
        let mut stored = workflow_job_spec(submitted_spec()).unwrap();
        stored.job_id = EnvironmentJobId::new("not a job id");
        assert!(wire_job_spec(stored).is_err());
    }

    #[test]
    fn request_ids_fit_the_id_grammar_for_any_call_id() {
        assert_eq!(
            workflow_tool_request_id(1, 2, 3, "toolu_01").as_str(),
            "jobreq:1:2:3:toolu_01"
        );
        let hashed = workflow_tool_request_id(1, 2, 3, "call id/with spaces");
        assert!(hashed.as_str().starts_with("jobreq-"));
        assert_eq!(
            hashed,
            workflow_tool_request_id(1, 2, 3, "call id/with spaces")
        );
        assert!(
            derived_job_run_id("invocation")
                .as_str()
                .starts_with("job-")
        );
    }

    #[test]
    fn job_group_ids_follow_the_encoded_payload() {
        let environment_id = EnvironmentId::new("environment_1");
        let payload = EnvironmentJobStartPayloadV1::new(
            "jobreq:1:1:1:call",
            vec![workflow_job_spec(submitted_spec()).unwrap()],
        );
        let encoded = payload.encode().unwrap();
        let first = derived_job_group_id(&environment_id, &BlobRef::from_bytes(&encoded));
        assert_eq!(
            first,
            derived_job_group_id(&environment_id, &BlobRef::from_bytes(&encoded))
        );
        let other = EnvironmentJobStartPayloadV1::new("jobreq:1:1:1:other", payload.jobs.clone());
        assert_ne!(
            first,
            derived_job_group_id(
                &environment_id,
                &BlobRef::from_bytes(&other.encode().unwrap())
            )
        );
    }

    #[test]
    fn not_found_entries_become_lost_observations() {
        let job_id = JobId::new("build").unwrap();
        assert_eq!(
            entry_error_observation(&job_id, ProtocolError::not_found("unknown job")).unwrap(),
            EnvironmentJobObservation::NotFound {
                job_id: EnvironmentJobId::new("build")
            }
        );
        assert!(entry_error_observation(&job_id, ProtocolError::internal("broken")).is_err());
    }

    #[test]
    fn refusals_are_terminal_and_transport_trouble_is_retried() {
        let retryable = |error: ClientError| is_transient(&error);
        assert!(!retryable(ClientError::Protocol(
            ProtocolError::failed_precondition(PreconditionReason::JobIdInUse, "job id in use")
        )));
        assert!(!retryable(ClientError::Protocol(
            ProtocolError::invalid_params("bad")
        )));
        assert!(retryable(ClientError::Protocol(
            ProtocolError::unavailable("daemon restarting")
        )));
        assert!(retryable(ClientError::Protocol(ProtocolError::internal(
            "bug"
        ))));
        assert!(retryable(ClientError::Connect {
            endpoint: "ws://gateway.test".to_owned(),
            message: "refused".to_owned(),
        }));
        assert!(retryable(ClientError::DeadlineExceeded {
            elapsed_ms: 60_000
        }));
    }

    #[test]
    fn daemon_summaries_map_onto_workflow_records() {
        let summary: JobSummary = serde_json::from_value(serde_json::json!({
            "jobId": "build",
            "requestId": "jobreq:1:1:1:call",
            "name": "build",
            "status": "timedOut",
            "dependsOn": ["setup"],
            "queueKey": "ci",
            "createdAtMs": 1,
            "stoppedBy": "timeout",
        }))
        .unwrap();
        assert_eq!(
            job_record(&summary),
            EnvironmentJobRecord {
                job_id: EnvironmentJobId::new("build"),
                name: Some("build".to_owned()),
                status: EnvironmentJobStatus::TimedOut,
                depends_on: vec![EnvironmentJobId::new("setup")],
                queue_key: Some("ci".to_owned()),
            }
        );
    }
}
