use std::collections::{BTreeMap, BTreeSet};

use engine::{BlobRef, PromiseSourceCheckResult, storage::BlobStore};
use environments::{EnvironmentId, EnvironmentJobGroupId};
use host_client::{HostClientError, HostDataClient};
use host_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        jobs::{CancelJobsParams, JobStatus, ReadJobsParams},
    },
    error::HostErrorCode,
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport},
};
use temporal_workflow::{
    EnvironmentJobCancelActivityRequest, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult, EnvironmentJobPrepareWorkflowToolRequest,
    EnvironmentJobStartActivityRequest, EnvironmentJobStartActivityResult,
    EnvironmentJobStartPayload, EnvironmentJobSubscription, EnvironmentJobWorkflowArgs,
    EnvironmentJobWorkflowToolContext,
};
use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;
use tools::environment::jobs::{
    JOB_RUN_WORKFLOW_SEMANTIC_TYPE, JOB_RUN_WORKFLOW_TOOL_ID, JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE,
    JOB_SUBMIT_WORKFLOW_TOOL_ID, JobHandle, normalize_job_result, store_model_job_result,
};

use super::{common::activity_error, state::EnvironmentJobActivityDeps};

const PROMISE_JOB_OUTPUT_BYTES: usize = 16 * 1024;

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
    let deps = deps.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
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
    let instance = deps
        .environments
        .read_environment(&environment_id)
        .await
        .map_err(activity_error)?;
    if execution_context
        .allowed_provider_ids
        .as_ref()
        .is_some_and(|providers| {
            !providers.iter().any(|provider| {
                instance
                    .provider_id()
                    .is_some_and(|id| provider == id.as_str())
            })
        })
    {
        return Err(activity_error(anyhow::anyhow!(
            "environment provider is not allowed for this session: {}",
            instance
                .provider_id()
                .map(ToString::to_string)
                .unwrap_or_else(|| "enrolled".to_owned())
        )));
    }
    let request_id = format!(
        "jobreq:{}:{}:{}:{}",
        start.invocation.run_id.as_u64(),
        start.invocation.turn_id.as_u64(),
        start.invocation.tool_batch_id.as_u64(),
        start.invocation.tool_call_id.as_str()
    );
    let jobs = match tool_kind {
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
                    spec.into_host_spec(job_id)
                        .map_err(|error| activity_error(anyhow::anyhow!(error.to_string())))
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        WorkflowJobToolKind::Run => {
            let args: tools::environment::jobs::JobRunArgs =
                serde_json::from_slice(&arguments).map_err(activity_error)?;
            let job_id = derived_job_run_id(start.invocation.invocation_id.as_str());
            vec![
                args.into_host_spec(job_id)
                    .map_err(|error| activity_error(anyhow::anyhow!(error.to_string())))?,
            ]
        }
    };
    let params = host_protocol::data::jobs::StartJobsParams {
        namespace: environment_id.as_str().to_owned(),
        request_id,
        jobs,
    };
    let request_fingerprint =
        BlobRef::from_bytes(&serde_json::to_vec(&params).map_err(activity_error)?);
    let job_group_id = derived_workflow_tool_job_group_id(
        &environment_id,
        &params.request_id,
        request_fingerprint.as_str(),
    );
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
            if completion_promises.len() != params.jobs.len() {
                return Err(activity_error(anyhow::anyhow!(
                    "job_submit completion promise count does not match job count"
                )));
            }
            params
                .jobs
                .iter()
                .enumerate()
                .map(|(index, job)| {
                    let completion_key = format!("job-{index}");
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
                .get(engine::REPLY_COMPLETION_KEY)
                .ok_or_else(|| {
                    activity_error(anyhow::anyhow!(
                        "job_run is missing completion key {}",
                        engine::REPLY_COMPLETION_KEY
                    ))
                })?;
            vec![EnvironmentJobSubscription {
                holder_workflow_id: start.holder_workflow_id.clone(),
                promise_id: promise_id.as_str().to_owned(),
                completion_key: engine::REPLY_COMPLETION_KEY.to_owned(),
                job_id: params.jobs[0].job_id.clone(),
                notified: false,
            }]
        }
    };
    let job_ids = params.jobs.iter().map(|job| job.job_id.clone()).collect();
    let payload = EnvironmentJobStartPayload { request: params };
    let request_ref = deps
        .blobs
        .put_bytes(serde_json::to_vec(&payload).map_err(activity_error)?)
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
        job_ids,
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

fn derived_job_run_id(invocation_id: &str) -> host_protocol::shared::JobId {
    let hash = BlobRef::from_bytes(invocation_id.as_bytes());
    host_protocol::shared::JobId::new(format!("job-{}", &hash.as_str()[7..31]))
}

fn derived_workflow_tool_job_group_id(
    environment_id: &EnvironmentId,
    request_id: &str,
    request_fingerprint: &str,
) -> EnvironmentJobGroupId {
    let hash = BlobRef::from_bytes(
        format!("{environment_id}:{request_id}:{request_fingerprint}").as_bytes(),
    );
    EnvironmentJobGroupId::new(format!("ejg_{}", &hash.as_str()[7..31]))
}

pub(super) async fn start(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobStartActivityRequest,
) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
    let deps = deps.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
    let environment_id =
        EnvironmentId::try_new(request.environment_id.clone()).map_err(activity_error)?;
    let mut payload: EnvironmentJobStartPayload = serde_json::from_slice(
        &deps
            .blobs
            .read_bytes(&request.request_ref)
            .await
            .map_err(activity_error)?,
    )
    .map_err(activity_error)?;
    if payload.request.namespace != environment_id.as_str() {
        return Err(activity_error(anyhow::anyhow!(
            "environment job start namespace does not match instance {environment_id}"
        )));
    }

    for job in &mut payload.request.jobs {
        let secret_env = deps
            .credentials
            .resolve_secret_env(&environment_id, &job.env)
            .await
            .map_err(activity_error)?;
        job.secret_env.extend(secret_env);
    }

    start_on_provider(deps, &environment_id, &payload).await
}

async fn start_on_provider(
    deps: &EnvironmentJobActivityDeps,
    environment_id: &EnvironmentId,
    payload: &EnvironmentJobStartPayload,
) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
    let instance = deps
        .environments
        .read_environment(environment_id)
        .await
        .map_err(activity_error)?;
    if matches!(
        instance.status,
        environments::EnvironmentStatus::Closing | environments::EnvironmentStatus::Closed
    ) {
        return Err(non_retryable_activity_error(anyhow::anyhow!(
            "cannot start jobs on closing environment instance {environment_id}"
        )));
    }
    let gateway = deps.gateway.as_ref().ok_or_else(|| {
        non_retryable_activity_error(anyhow::anyhow!(
            "environment gateway is not configured on this worker"
        ))
    })?;
    let connection = gateway.connection_for(deps.universe_id, &instance);
    let (mut client, capabilities) = initialized_client(&connection, gateway).await?;
    if !capabilities.job_start {
        return Err(non_retryable_activity_error(anyhow::anyhow!(
            "environment does not support durable job start: {environment_id}"
        )));
    }
    let response = client
        .start_jobs(&payload.request)
        .await
        .map_err(start_host_activity_error)?;
    let requested_ids = payload
        .request
        .jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let returned_ids = response
        .jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    if requested_ids != returned_ids {
        return Err(non_retryable_activity_error(anyhow::anyhow!(
            "environment provider start response job ids do not match the request"
        )));
    }
    Ok(EnvironmentJobStartActivityResult {
        jobs: response.jobs,
    })
}

fn start_host_activity_error(error: HostClientError) -> ActivityError {
    match &error {
        HostClientError::Host(error)
            if matches!(
                error.code,
                HostErrorCode::InvalidRequest
                    | HostErrorCode::Unauthorized
                    | HostErrorCode::Forbidden
                    | HostErrorCode::NotFound
                    | HostErrorCode::Conflict
                    | HostErrorCode::Unsupported
                    | HostErrorCode::CapabilityUnavailable
            ) =>
        {
            non_retryable_activity_error(anyhow::anyhow!(error.message.clone()))
        }
        _ => activity_error(error),
    }
}

fn non_retryable_activity_error(error: impl Into<anyhow::Error>) -> ActivityError {
    ActivityError::application(ApplicationFailure::non_retryable(error.into()))
}

pub(super) async fn poll(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobPollActivityRequest,
) -> Result<EnvironmentJobPollActivityResult, ActivityError> {
    let deps = deps.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
    let environment_id = EnvironmentId::try_new(request.environment_id).map_err(activity_error)?;
    let instance = deps
        .environments
        .read_environment(&environment_id)
        .await
        .map_err(activity_error)?;
    let gateway = deps.gateway.as_ref().ok_or_else(|| {
        non_retryable_activity_error(anyhow::anyhow!(
            "environment gateway is not configured on this worker"
        ))
    })?;
    let connection = gateway.connection_for(deps.universe_id, &instance);
    let (mut client, _) = initialized_client(&connection, gateway).await?;
    let requested_job_ids = request.job_ids.iter().cloned().collect::<BTreeSet<_>>();
    let response = client
        .read_jobs(&ReadJobsParams {
            namespace: environment_id.as_str().to_owned(),
            jobs: request.job_ids,
            after_seq: None,
            max_bytes: Some(PROMISE_JOB_OUTPUT_BYTES),
            include_artifacts: true,
            wait_ms: None,
        })
        .await
        .map_err(activity_error)?;
    let mut jobs = Vec::with_capacity(response.jobs.len());
    let mut resolutions = BTreeMap::new();
    for result in response.jobs {
        let summary = result.summary.clone();
        if summary.status.is_terminal() {
            let error = (summary.status != JobStatus::Succeeded).then(|| {
                summary.failure.clone().unwrap_or_else(|| {
                    format!(
                        "environment job {} ended as {:?}",
                        summary.job_id, summary.status
                    )
                })
            });
            let normalized = normalize_job_result(
                deps.blobs.as_ref(),
                Some(JobHandle {
                    environment_id: environment_id.as_str().to_owned(),
                    job_id: summary.job_id.clone(),
                }),
                Some(result.summary),
                result.output_chunks,
                result.output_next_seq,
                result.artifacts,
                error,
                Some(PROMISE_JOB_OUTPUT_BYTES),
            )
            .await
            .map_err(activity_error)?;
            let result_ref = store_model_job_result(
                deps.blobs.as_ref(),
                deps.blob_graph.as_deref(),
                &normalized,
            )
            .await
            .map_err(activity_error)?;
            let resolution = if summary.status == JobStatus::Succeeded {
                PromiseSourceCheckResult::Resolved {
                    payload_ref: Some(result_ref),
                }
            } else {
                PromiseSourceCheckResult::Failed {
                    error_ref: Some(result_ref),
                }
            };
            resolutions.insert(summary.job_id.as_str().to_owned(), resolution);
        }
        jobs.push(summary);
    }
    let returned_job_ids = jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let terminal = returned_job_ids == requested_job_ids
        && !jobs.is_empty()
        && jobs.iter().all(|job| job.status.is_terminal());
    Ok(EnvironmentJobPollActivityResult {
        jobs,
        resolutions,
        terminal,
    })
}

pub(super) async fn cancel(
    deps: Option<&EnvironmentJobActivityDeps>,
    request: EnvironmentJobCancelActivityRequest,
) -> Result<Vec<host_protocol::data::jobs::JobSummary>, ActivityError> {
    let deps = deps.ok_or_else(|| {
        activity_error(anyhow::anyhow!(
            "environment job activities are not configured"
        ))
    })?;
    let environment_id = EnvironmentId::try_new(request.environment_id).map_err(activity_error)?;
    let instance = deps
        .environments
        .read_environment(&environment_id)
        .await
        .map_err(activity_error)?;
    let gateway = deps.gateway.as_ref().ok_or_else(|| {
        non_retryable_activity_error(anyhow::anyhow!(
            "environment gateway is not configured on this worker"
        ))
    })?;
    let connection = gateway.connection_for(deps.universe_id, &instance);
    let (mut client, _) = initialized_client(&connection, gateway).await?;
    let response = client
        .cancel_jobs(&CancelJobsParams {
            namespace: environment_id.as_str().to_owned(),
            jobs: request.jobs,
            scope: request.scope,
            force: request.force,
        })
        .await
        .map_err(activity_error)?;
    Ok(response.jobs)
}

async fn initialized_client(
    connection: &HostConnectionSpec,
    gateway: &crate::environment_gateway::EnvironmentGatewayClientConfig,
) -> Result<
    (
        HostDataClient<host_client::WebSocketTransport>,
        host_protocol::shared::HostCapabilities,
    ),
    ActivityError,
> {
    if connection.transport != HostTransport::WebSocket {
        return Err(activity_error(anyhow::anyhow!(
            "unsupported environment job transport: {:?}",
            connection.transport
        )));
    }
    let mut client = HostDataClient::connect(
        &connection.endpoint,
        gateway.connect_options("lightspeed-environment-job-workflow"),
    )
    .await
    .map_err(activity_error)?;
    let response = client
        .initialize(&InitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-environment-job-workflow".to_owned(),
            scope: connection.scope.clone(),
            resume_connection_id: None,
        })
        .await
        .map_err(activity_error)?;
    if response.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(activity_error(anyhow::anyhow!(
            "unsupported host protocol version {}",
            response.protocol_version
        )));
    }
    let capabilities = response.capabilities;
    client
        .initialized(&InitializedParams {})
        .await
        .map_err(activity_error)?;
    Ok((client, capabilities))
}
