use super::*;

use ::environments::{EnvironmentId, EnvironmentJobGroupId, EnvironmentRecord};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use environment_client::{ClientError, DataClient};
use environment_protocol::{
    data::{
        exec::{
            ExitStatus as ProtocolExitStatus, GapReason as ProtocolGapReason,
            OutputChunk as ProtocolOutputChunk, OutputGap as ProtocolOutputGap,
            OutputStream as ProtocolOutputStream, ReturnOn as ProtocolReturnOn,
            StoppedBy as ProtocolStoppedBy,
        },
        handshake::JobLimits,
        job::{
            CancelJobsParams as ProtocolCancelJobsParams, JobCancelScope as ProtocolJobCancelScope,
            JobReadEntry as ProtocolJobReadEntry, JobReadRequest as ProtocolJobReadRequest,
            JobStatus as ProtocolJobStatus, JobSummary as ProtocolJobSummary,
            JobSummaryEntry as ProtocolJobSummaryEntry, ReadJobsParams as ProtocolReadJobsParams,
        },
    },
    error::ProtocolErrorKind,
    ids::{JobId, JobName, JobRequestId, QueueKey},
    paths::EnvironmentPath,
};
use temporal_workflow::{
    EnvironmentJobCommand, EnvironmentJobDependency, EnvironmentJobDependencyPolicy,
    EnvironmentJobId, EnvironmentJobRecord, EnvironmentJobSpec, EnvironmentJobStartPayloadV1,
    EnvironmentJobStatus,
};
use tools::environment::jobs::DEFAULT_JOB_OUTPUT_BYTES;

/// `User-Agent` of the job API's worker-route upgrades; `clientInfo` is
/// always the runtime build.
const JOB_API_USER_AGENT: &str = "lightspeed-temporal-runtime";

/// Deadline of each data-plane call the job API makes.
const JOB_API_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

impl GatewayAgentApi {
    pub(super) async fn create_environment_job_records(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        let environment_id = parse_environment_job_environment_id(params.environment_id)?;
        let request_id = JobRequestId::new(params.request_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid request_id: {error}"))
        })?;
        if params.jobs.is_empty() {
            return Err(AgentApiError::invalid_request(
                "environments/jobs/create requires at least one job",
            ));
        }
        let mut specs = Vec::with_capacity(params.jobs.len());
        for (index, spec) in params.jobs.into_iter().enumerate() {
            let job_id = spec
                .job_id
                .as_deref()
                .map(parse_protocol_job_id)
                .transpose()
                .map_err(AgentApiError::invalid_request)?
                .unwrap_or_else(|| derived_job_id(&environment_id, &request_id, index));
            specs.push(
                api_start_spec_to_workflow(spec, job_id).map_err(AgentApiError::invalid_request)?,
            );
        }
        let instance = self.read_job_instance(&environment_id).await?;
        // Refuse what the daemon would refuse before a workflow exists for
        // it: a daemon without jobs, or more jobs than one start takes.
        let client = self.connect_job_client(&instance).await?;
        let limits = client.initialize_response().limits.job.clone();
        let _ = client.close().await;
        if specs.len() > limits.max_request_jobs as usize {
            return Err(AgentApiError::invalid_request(format!(
                "environments/jobs/create names {} jobs; the environment starts at most {} at once",
                specs.len(),
                limits.max_request_jobs
            )));
        }
        self.start_environment_jobs(instance, request_id, specs)
            .await
    }

    async fn start_environment_jobs(
        &self,
        instance: EnvironmentRecord,
        request_id: JobRequestId,
        jobs: Vec<EnvironmentJobSpec>,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        let payload = EnvironmentJobStartPayloadV1::new(request_id.into_string(), jobs);
        let encoded = payload.encode().map_err(|error| {
            AgentApiError::internal(format!("encode environment job workflow start: {error}"))
        })?;
        let job_group_id =
            derived_job_group_id(&instance.environment_id, &BlobRef::from_bytes(&encoded));
        let request_ref = self
            .store
            .put_bytes(encoded)
            .await
            .map_err(map_blob_store_error)?;
        let snapshot = self
            .start_environment_job_workflow(
                temporal_workflow::EnvironmentJobStartActivityRequest {
                    universe_id: self.universe_id(),
                    environment_id: instance.environment_id.as_str().to_owned(),
                    job_group_id: job_group_id.as_str().to_owned(),
                    request_ref,
                },
                payload.job_ids(),
                Vec::new(),
            )
            .await?;
        Ok(EnvironmentJobCreateResponse {
            environment_id: instance.environment_id.as_str().to_owned(),
            job_group_id: job_group_id.as_str().to_owned(),
            jobs: snapshot
                .jobs
                .into_iter()
                .map(|record| api_started_job(&instance.environment_id, record))
                .collect(),
        })
    }

    async fn start_environment_job_workflow(
        &self,
        start: temporal_workflow::EnvironmentJobStartActivityRequest,
        job_ids: Vec<EnvironmentJobId>,
        subscriptions: Vec<temporal_workflow::EnvironmentJobSubscription>,
    ) -> Result<temporal_workflow::EnvironmentJobWorkflowSnapshot, AgentApiError> {
        let workflow_id = temporal_workflow::compose_environment_job_workflow_id(
            self.universe_id(),
            &start.environment_id,
            &start.job_group_id,
        );
        match self
            .client
            .start_workflow(
                temporal_workflow::EnvironmentJobWorkflow::run,
                temporal_workflow::EnvironmentJobWorkflowInput::Job(
                    temporal_workflow::EnvironmentJobWorkflowArgs {
                        universe_id: self.universe_id(),
                        start,
                        job_ids,
                        subscriptions,
                        started: false,
                        jobs: Vec::new(),
                        resolutions: BTreeMap::new(),
                        poll_ms: 2_000,
                        poll_attempt: 0,
                        workflow_tool: None,
                    },
                ),
                WorkflowStartOptions::new(self.task_queue.clone(), workflow_id.clone()).build(),
            )
            .await
        {
            Ok(_) => {}
            Err(error) => {
                let error = map_workflow_start_error(error);
                if error.kind != AgentApiErrorKind::Conflict {
                    return Err(error);
                }
            }
        }
        let handle = self
            .client
            .get_workflow_handle::<temporal_workflow::EnvironmentJobWorkflow>(workflow_id);
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(
                    "timed out waiting for environment job start",
                ));
            }
            match handle
                .query(
                    temporal_workflow::EnvironmentJobWorkflow::snapshot,
                    (),
                    WorkflowQueryOptions::default(),
                )
                .await
            {
                Ok(snapshot) if snapshot.started => return Ok(snapshot),
                Ok(snapshot) if snapshot.last_error.is_some() => {
                    return Err(AgentApiError::internal(snapshot.last_error.unwrap_or_else(
                        || "environment job workflow start failed".to_owned(),
                    )));
                }
                Ok(_) | Err(WorkflowQueryError::NotFound(_)) => {}
                Err(error) => return Err(map_workflow_query_error(error)),
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn read_environment_job_records(
        &self,
        params: EnvironmentJobReadParams,
    ) -> Result<EnvironmentJobReadResponse, AgentApiError> {
        if params.after.is_some() && params.tail_bytes.is_some() {
            return Err(AgentApiError::invalid_request(
                "environments/jobs/read takes after or tailBytes, not both",
            ));
        }
        let position = JobReadPosition {
            after: params.after,
            tail_bytes: params.tail_bytes,
            max_bytes: params.max_bytes.unwrap_or(DEFAULT_JOB_OUTPUT_BYTES),
        };
        let mut entries: Vec<Option<SessionJobReadEntryView>> = vec![None; params.jobs.len()];
        let mut groups: BTreeMap<EnvironmentId, Vec<(usize, SessionJobHandleView, JobId)>> =
            BTreeMap::new();
        for (index, handle) in params.jobs.into_iter().enumerate() {
            match parse_job_handle(handle) {
                Ok((environment_id, handle, job_id)) => groups
                    .entry(environment_id)
                    .or_default()
                    .push((index, handle, job_id)),
                Err(error) => entries[index] = Some(session_job_read_error(None, error)),
            }
        }
        for (environment_id, jobs) in groups {
            match self
                .read_jobs_in_environment(&environment_id, &jobs, position)
                .await
            {
                Ok(read) => {
                    for ((index, _, _), entry) in jobs.iter().zip(read) {
                        entries[*index] = Some(entry);
                    }
                }
                Err(error) => {
                    for (index, handle, _) in jobs {
                        entries[index] = Some(session_job_read_error(Some(handle), error.clone()));
                    }
                }
            }
        }
        Ok(EnvironmentJobReadResponse {
            jobs: entries.into_iter().flatten().collect(),
        })
    }

    /// One `job/read` entry per job, in order, within the daemon's ceilings.
    async fn read_jobs_in_environment(
        &self,
        environment_id: &EnvironmentId,
        jobs: &[(usize, SessionJobHandleView, JobId)],
        position: JobReadPosition,
    ) -> Result<Vec<SessionJobReadEntryView>, String> {
        let instance = self
            .read_job_instance(environment_id)
            .await
            .map_err(|error| error.message)?;
        let client = self
            .connect_job_client(&instance)
            .await
            .map_err(|error| error.message)?;
        let limits = client.initialize_response().limits.job.clone();
        let mut entries = Vec::with_capacity(jobs.len());
        let mut result = Ok(());
        for chunk in jobs.chunks((limits.max_request_jobs as usize).max(1)) {
            let params =
                job_read_params(chunk.iter().map(|(_, _, job_id)| job_id), position, &limits);
            match client.read_jobs(&params).await {
                Ok(response) => {
                    for ((_, handle, _), entry) in chunk.iter().zip(response.jobs) {
                        entries.push(session_job_read_entry(handle.clone(), entry));
                    }
                }
                Err(error) => {
                    result = Err(error.to_string());
                    break;
                }
            }
        }
        let _ = client.close().await;
        result.map(|()| entries)
    }

    pub(super) async fn cancel_environment_job_records(
        &self,
        params: EnvironmentJobCancelParams,
    ) -> Result<EnvironmentJobCancelResponse, AgentApiError> {
        let scope = match params.scope {
            SessionJobCancelScopeView::Job => ProtocolJobCancelScope::Job,
            SessionJobCancelScopeView::Dependents => ProtocolJobCancelScope::Dependents,
        };
        let mut entries = Vec::with_capacity(params.jobs.len());
        let mut groups: BTreeMap<EnvironmentId, Vec<(SessionJobHandleView, JobId)>> =
            BTreeMap::new();
        for handle in params.jobs {
            match parse_job_handle(handle) {
                Ok((environment_id, handle, job_id)) => groups
                    .entry(environment_id)
                    .or_default()
                    .push((handle, job_id)),
                Err(error) => entries.push(session_job_cancel_error(None, error)),
            }
        }
        for (environment_id, jobs) in groups {
            match self
                .cancel_jobs_in_environment(&environment_id, &jobs, scope, params.force)
                .await
            {
                Ok(cancelled) => entries.extend(cancelled),
                Err(error) => entries.extend(
                    jobs.into_iter()
                        .map(|(handle, _)| session_job_cancel_error(Some(handle), error.clone())),
                ),
            }
        }
        Ok(EnvironmentJobCancelResponse { jobs: entries })
    }

    /// The daemon's entries for the named jobs, then for the dependents a
    /// `dependents` cancel reached.
    async fn cancel_jobs_in_environment(
        &self,
        environment_id: &EnvironmentId,
        jobs: &[(SessionJobHandleView, JobId)],
        scope: ProtocolJobCancelScope,
        force: bool,
    ) -> Result<Vec<SessionJobCancelEntryView>, String> {
        let instance = self
            .read_job_instance(environment_id)
            .await
            .map_err(|error| error.message)?;
        let client = self
            .connect_job_client(&instance)
            .await
            .map_err(|error| error.message)?;
        let per_request =
            (client.initialize_response().limits.job.max_request_jobs as usize).max(1);
        let mut entries = Vec::new();
        let mut result = Ok(());
        for chunk in jobs.chunks(per_request) {
            let params = ProtocolCancelJobsParams {
                job_ids: chunk.iter().map(|(_, job_id)| job_id.clone()).collect(),
                scope,
                force,
            };
            match client.cancel_jobs(&params).await {
                Ok(response) => entries.extend(
                    response
                        .jobs
                        .into_iter()
                        .map(|entry| session_job_cancel_entry(environment_id, entry)),
                ),
                Err(error) => {
                    result = Err(error.to_string());
                    break;
                }
            }
        }
        let _ = client.close().await;
        result.map(|()| entries)
    }

    /// Resolve the environment through the shared use gate, so a
    /// provisioning environment or a reconnecting daemon fails typed
    /// `environment_not_ready` for the caller to retry — the shape polling
    /// automations lean on — and a closed or failed one is refused.
    async fn read_job_instance(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, AgentApiError> {
        crate::environments::resolver::EnvironmentResolver::from_pg_store(self.store.clone())
            .with_gateway(self.environment_gateway.clone())
            .resolve_for_connection(environment_id, super::now_ms()?)
            .await
            .map_err(super::environments::map_environment_resolve_error)
    }

    /// An initialized data client for the environment; its daemon must
    /// serve the `job` family.
    async fn connect_job_client(
        &self,
        instance: &EnvironmentRecord,
    ) -> Result<DataClient, AgentApiError> {
        let key = self
            .environment_gateway
            .route_key(self.store.config().universe_id, instance);
        let (client, initialized) = DataClient::connect_initialized(
            &self.environment_gateway.endpoint(&key),
            self.environment_gateway
                .connect_options(JOB_API_USER_AGENT)
                .with_request_timeout(JOB_API_REQUEST_TIMEOUT),
        )
        .await
        .map_err(|error| {
            // A powered-down environment wakes on this dial; its typed
            // refusal is `environment_not_ready`, like the gate's waits.
            match crate::environments::resolver::route_refusal_error(instance, &error) {
                Some(refusal) => super::environments::map_environment_resolve_error(refusal),
                None => map_environment_client_api_error(error),
            }
        })?;
        if initialized.capabilities.job.is_none() {
            let _ = client.close().await;
            return Err(AgentApiError::rejected(format!(
                "environment does not serve durable jobs: {}",
                instance.environment_id
            )));
        }
        Ok(client)
    }
}

/// Where each job of an API read starts and how much it shows.
#[derive(Clone, Copy, Debug)]
struct JobReadPosition {
    after: Option<u64>,
    tail_bytes: Option<u64>,
    /// Per job; never more than the daemon's read ceiling allows.
    max_bytes: u64,
}

/// One `job/read` for `jobs` at `position`, waiting for nothing. The
/// per-job budget shrinks so the total stays within `maxReadBytes`.
fn job_read_params<'a>(
    jobs: impl ExactSizeIterator<Item = &'a JobId>,
    position: JobReadPosition,
    limits: &JobLimits,
) -> ProtocolReadJobsParams {
    let count = jobs.len().max(1) as u64;
    let per_job = position.max_bytes.min(limits.max_read_bytes / count);
    ProtocolReadJobsParams {
        jobs: jobs
            .map(|job_id| ProtocolJobReadRequest {
                job_id: job_id.clone(),
                after: position.after,
                tail_bytes: position.tail_bytes,
                max_bytes: Some(per_job),
            })
            .collect(),
        max_bytes: per_job * count,
        wait_ms: 0,
        return_on: ProtocolReturnOn::AnyOutput,
    }
}

fn parse_environment_job_environment_id(value: String) -> Result<EnvironmentId, AgentApiError> {
    EnvironmentId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid environment instance id: {error}"))
    })
}

fn parse_protocol_job_id(value: &str) -> Result<JobId, String> {
    JobId::new(value).map_err(|error| format!("invalid job_id: {error}"))
}

fn parse_job_handle(
    handle: SessionJobHandleInput,
) -> Result<(EnvironmentId, SessionJobHandleView, JobId), String> {
    let environment_id = EnvironmentId::try_new(handle.environment_id)
        .map_err(|error| format!("invalid job handle environment_id: {error}"))?;
    let job_id = parse_protocol_job_id(&handle.job_id)?;
    let view = SessionJobHandleView {
        environment_id: environment_id.as_str().to_owned(),
        job_id: job_id.as_str().to_owned(),
    };
    Ok((environment_id, view, job_id))
}

/// The job workflow's record of an API job spec, validated against the
/// wire grammar so the start activity cannot refuse it later.
fn api_start_spec_to_workflow(
    spec: SessionJobStartSpecInput,
    job_id: JobId,
) -> Result<EnvironmentJobSpec, String> {
    if spec.argv.is_empty() {
        return Err("job argv must not be empty".to_owned());
    }
    let name = spec
        .name
        .map(|name| {
            JobName::new(name)
                .map(JobName::into_string)
                .map_err(|error| format!("invalid job name: {error}"))
        })
        .transpose()?;
    let cwd = spec
        .cwd
        .map(|cwd| {
            EnvironmentPath::new(cwd)
                .map(|cwd| cwd.as_str().to_owned())
                .map_err(|error| format!("invalid job cwd: {error}"))
        })
        .transpose()?;
    let queue_key = spec
        .queue_key
        .map(|key| {
            QueueKey::new(key)
                .map(QueueKey::into_string)
                .map_err(|error| format!("invalid job queue_key: {error}"))
        })
        .transpose()?;
    Ok(EnvironmentJobSpec {
        job_id: EnvironmentJobId::new(job_id.into_string()),
        name,
        command: EnvironmentJobCommand {
            argv: spec.argv,
            cwd,
            env: spec.env,
            stdin: spec.stdin,
            timeout_ms: spec.timeout_ms,
        },
        depends_on: spec
            .depends_on
            .into_iter()
            .map(api_dependency_to_workflow)
            .collect::<Result<Vec<_>, _>>()?,
        dependency_policy: match spec.dependency_policy {
            SessionJobDependencyPolicyView::AllSucceeded => {
                EnvironmentJobDependencyPolicy::AllSucceeded
            }
            SessionJobDependencyPolicyView::AllTerminal => {
                EnvironmentJobDependencyPolicy::AllTerminal
            }
        },
        queue_key,
    })
}

fn api_dependency_to_workflow(
    dependency: SessionJobDependencyInput,
) -> Result<EnvironmentJobDependency, String> {
    match (dependency.job_id, dependency.name) {
        (Some(job_id), None) => Ok(EnvironmentJobDependency::JobId {
            job_id: EnvironmentJobId::new(parse_protocol_job_id(&job_id)?.into_string()),
        }),
        (None, Some(name)) => Ok(EnvironmentJobDependency::Name {
            name: JobName::new(name)
                .map_err(|error| format!("invalid job dependency name: {error}"))?
                .into_string(),
        }),
        (Some(_), Some(_)) => {
            Err("job dependency must specify job_id or name, not both".to_owned())
        }
        (None, None) => Err("job dependency must specify job_id or name".to_owned()),
    }
}

fn derived_job_id(
    environment_id: &EnvironmentId,
    request_id: &JobRequestId,
    index: usize,
) -> JobId {
    let hash = BlobRef::from_bytes(format!("{environment_id}:{request_id}:{index}").as_bytes());
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

fn map_environment_client_api_error(error: ClientError) -> AgentApiError {
    let message = error.to_string();
    match error.protocol_kind() {
        Some(ProtocolErrorKind::InvalidParams | ProtocolErrorKind::InvalidRequest) => {
            AgentApiError::invalid_request(message)
        }
        Some(ProtocolErrorKind::NotFound) => AgentApiError::not_found(message),
        Some(ProtocolErrorKind::AlreadyExists) => AgentApiError::conflict(message),
        Some(
            ProtocolErrorKind::FailedPrecondition
            | ProtocolErrorKind::CapabilityUnavailable
            | ProtocolErrorKind::PermissionDenied
            | ProtocolErrorKind::VersionMismatch,
        ) => AgentApiError::rejected(message),
        _ => match error {
            ClientError::WrongPlane { .. } | ClientError::Configuration { .. } => {
                AgentApiError::rejected(message)
            }
            _ => AgentApiError::internal(message),
        },
    }
}

/// The started view of a job the workflow recorded. A job the daemon no
/// longer knows (`lost`) is reported as failed: the API has no such status.
fn api_started_job(
    environment_id: &EnvironmentId,
    record: EnvironmentJobRecord,
) -> SessionJobStartedView {
    SessionJobStartedView {
        name: record.name,
        job_id: record.job_id.as_str().to_owned(),
        handle: SessionJobHandleView {
            environment_id: environment_id.as_str().to_owned(),
            job_id: record.job_id.as_str().to_owned(),
        },
        promise_id: None,
        status: api_record_status(record.status),
        dependencies: record
            .depends_on
            .into_iter()
            .map(EnvironmentJobId::into_string)
            .collect(),
        queue_key: record.queue_key,
    }
}

fn api_record_status(status: EnvironmentJobStatus) -> SessionJobStatusView {
    match status {
        EnvironmentJobStatus::Queued => SessionJobStatusView::Queued,
        EnvironmentJobStatus::Running => SessionJobStatusView::Running,
        EnvironmentJobStatus::Succeeded => SessionJobStatusView::Succeeded,
        EnvironmentJobStatus::Failed | EnvironmentJobStatus::Lost => SessionJobStatusView::Failed,
        EnvironmentJobStatus::TimedOut => SessionJobStatusView::TimedOut,
        EnvironmentJobStatus::Cancelled => SessionJobStatusView::Cancelled,
        EnvironmentJobStatus::DependencyFailed => SessionJobStatusView::DependencyFailed,
        EnvironmentJobStatus::Interrupted => SessionJobStatusView::Interrupted,
    }
}

fn session_job_read_entry(
    handle: SessionJobHandleView,
    entry: ProtocolJobReadEntry,
) -> SessionJobReadEntryView {
    match entry {
        ProtocolJobReadEntry::Job { summary, output } => SessionJobReadEntryView {
            handle: Some(handle),
            summary: Some(api_job_summary(*summary)),
            output_chunks: output
                .chunks
                .into_iter()
                .map(api_job_output_chunk)
                .collect(),
            output_next: output.next,
            output_end: output.end,
            output_gaps: output.gaps.into_iter().map(api_job_output_gap).collect(),
            error: None,
        },
        ProtocolJobReadEntry::Error { error, .. } => {
            session_job_read_error(Some(handle), error.to_string())
        }
    }
}

fn session_job_read_error(
    handle: Option<SessionJobHandleView>,
    error: String,
) -> SessionJobReadEntryView {
    SessionJobReadEntryView {
        handle,
        summary: None,
        output_chunks: Vec::new(),
        output_next: 0,
        output_end: 0,
        output_gaps: Vec::new(),
        error: Some(error),
    }
}

fn session_job_cancel_entry(
    environment_id: &EnvironmentId,
    entry: ProtocolJobSummaryEntry,
) -> SessionJobCancelEntryView {
    let handle = |job_id: &JobId| SessionJobHandleView {
        environment_id: environment_id.as_str().to_owned(),
        job_id: job_id.as_str().to_owned(),
    };
    match entry {
        ProtocolJobSummaryEntry::Job { summary } => SessionJobCancelEntryView {
            handle: Some(handle(&summary.job_id)),
            summary: Some(api_job_summary(*summary)),
            error: None,
        },
        ProtocolJobSummaryEntry::Error { job_id, error } => {
            session_job_cancel_error(Some(handle(&job_id)), error.to_string())
        }
    }
}

fn session_job_cancel_error(
    handle: Option<SessionJobHandleView>,
    error: String,
) -> SessionJobCancelEntryView {
    SessionJobCancelEntryView {
        handle,
        summary: None,
        error: Some(error),
    }
}

fn api_job_summary(summary: ProtocolJobSummary) -> SessionJobSummaryView {
    SessionJobSummaryView {
        job_id: summary.job_id.into_string(),
        request_id: summary.request_id.into_string(),
        name: summary.name.map(JobName::into_string),
        status: api_job_status(summary.status),
        dependencies: summary
            .depends_on
            .into_iter()
            .map(JobId::into_string)
            .collect(),
        created_at_ms: summary.created_at_ms,
        queued_at_ms: summary.queued_at_ms,
        started_at_ms: summary.started_at_ms,
        finished_at_ms: summary.finished_at_ms,
        cancel_requested_at_ms: summary.cancel_requested_at_ms,
        duration_ms: summary.duration_ms,
        exit: summary.exit.map(|exit| match exit {
            ProtocolExitStatus::Code { code } => SessionJobExitView::Code { code },
            ProtocolExitStatus::Signal {
                signal,
                core_dumped,
            } => SessionJobExitView::Signal {
                signal: signal.as_str().to_owned(),
                core_dumped,
            },
        }),
        stopped_by: summary.stopped_by.map(|stopped_by| match stopped_by {
            ProtocolStoppedBy::Timeout => SessionJobStoppedByView::Timeout,
            ProtocolStoppedBy::Terminate => SessionJobStoppedByView::Terminate,
            ProtocolStoppedBy::Kill => SessionJobStoppedByView::Kill,
        }),
        orphaned_descendants: summary.leftovers.is_some(),
        failure: summary.error.map(|error| error.to_string()),
        queue_key: summary.queue_key.map(QueueKey::into_string),
    }
}

fn api_job_status(status: ProtocolJobStatus) -> SessionJobStatusView {
    match status {
        ProtocolJobStatus::Queued => SessionJobStatusView::Queued,
        ProtocolJobStatus::Running => SessionJobStatusView::Running,
        ProtocolJobStatus::Succeeded => SessionJobStatusView::Succeeded,
        ProtocolJobStatus::Failed => SessionJobStatusView::Failed,
        ProtocolJobStatus::TimedOut => SessionJobStatusView::TimedOut,
        ProtocolJobStatus::Cancelled => SessionJobStatusView::Cancelled,
        ProtocolJobStatus::DependencyFailed => SessionJobStatusView::DependencyFailed,
        ProtocolJobStatus::Interrupted => SessionJobStatusView::Interrupted,
    }
}

fn api_job_output_chunk(chunk: ProtocolOutputChunk) -> SessionJobOutputChunkView {
    SessionJobOutputChunkView {
        offset: chunk.offset,
        stream: match chunk.stream {
            // Jobs never run on a PTY; a merged stream reads as stdout.
            ProtocolOutputStream::Stdout | ProtocolOutputStream::Pty => {
                SessionJobOutputStreamView::Stdout
            }
            ProtocolOutputStream::Stderr => SessionJobOutputStreamView::Stderr,
        },
        data_base64: BASE64_STANDARD.encode(chunk.data.as_slice()),
    }
}

fn api_job_output_gap(gap: ProtocolOutputGap) -> SessionJobOutputGapView {
    SessionJobOutputGapView {
        offset: gap.offset,
        omitted_bytes: gap.omitted_bytes,
        reason: match gap.reason {
            ProtocolGapReason::Overflow => SessionJobOutputGapReasonView::Overflow,
            ProtocolGapReason::Expired => SessionJobOutputGapReasonView::Expired,
        },
    }
}

#[cfg(test)]
mod tests {
    use environment_protocol::{bytes::ByteChunk, data::exec::OutputPage, error::ProtocolError};

    use super::*;

    fn limits(max_read_bytes: u64) -> JobLimits {
        JobLimits {
            max_read_bytes,
            max_request_jobs: 64,
            max_retained_output_bytes: 2 * 1024 * 1024,
            record_retention_ms: 1,
            cancel_grace_ms: 1,
        }
    }

    fn job_id(value: &str) -> JobId {
        JobId::new(value).unwrap()
    }

    #[test]
    fn reads_split_the_daemon_ceiling_across_jobs() {
        let jobs = [job_id("a"), job_id("b")];
        let params = job_read_params(
            jobs.iter(),
            JobReadPosition {
                after: None,
                tail_bytes: Some(100),
                max_bytes: 1_000,
            },
            &limits(600),
        );
        assert_eq!(params.max_bytes, 600);
        assert_eq!(params.wait_ms, 0);
        for (entry, expected) in params.jobs.iter().zip(&jobs) {
            assert_eq!(&entry.job_id, expected);
            assert_eq!(entry.tail_bytes, Some(100));
            assert_eq!(entry.after, None);
            assert_eq!(entry.max_bytes, Some(300));
        }
    }

    #[test]
    fn api_specs_are_validated_against_the_wire_grammar() {
        let spec = |name: Option<&str>, cwd: Option<&str>| SessionJobStartSpecInput {
            name: name.map(str::to_owned),
            job_id: None,
            argv: vec!["make".to_owned()],
            cwd: cwd.map(str::to_owned),
            env: BTreeMap::new(),
            stdin: Some("input".to_owned()),
            timeout_ms: None,
            depends_on: vec![SessionJobDependencyInput {
                job_id: None,
                name: Some("lint".to_owned()),
            }],
            dependency_policy: SessionJobDependencyPolicyView::AllTerminal,
            queue_key: Some("ci".to_owned()),
        };
        let stored =
            api_start_spec_to_workflow(spec(Some("build"), Some("/workspace")), job_id("build"))
                .unwrap();
        assert_eq!(stored.command.cwd.as_deref(), Some("/workspace"));
        assert_eq!(stored.command.stdin.as_deref(), Some("input"));
        assert_eq!(
            stored.depends_on,
            vec![EnvironmentJobDependency::Name {
                name: "lint".to_owned()
            }]
        );
        assert!(api_start_spec_to_workflow(spec(None, Some("relative")), job_id("x")).is_err());
        assert!(api_start_spec_to_workflow(spec(Some("a\tb"), None), job_id("x")).is_err());
        assert!(parse_protocol_job_id("not a job id").is_err());
    }

    #[test]
    fn read_entries_carry_byte_positions_and_entry_errors() {
        let summary: ProtocolJobSummary = serde_json::from_value(serde_json::json!({
            "jobId": "build",
            "requestId": "req-1",
            "status": "failed",
            "createdAtMs": 1,
            "exit": {"kind": "signal", "signal": "SIGSEGV", "coreDumped": true},
            "stoppedBy": "kill",
        }))
        .unwrap();
        let handle = SessionJobHandleView {
            environment_id: "environment_1".to_owned(),
            job_id: "build".to_owned(),
        };
        let entry = session_job_read_entry(
            handle.clone(),
            ProtocolJobReadEntry::Job {
                summary: Box::new(summary),
                output: OutputPage {
                    chunks: vec![ProtocolOutputChunk {
                        offset: 10,
                        stream: ProtocolOutputStream::Stderr,
                        data: ByteChunk::from(b"boom".to_vec()),
                    }],
                    next: 14,
                    end: 40,
                    gaps: vec![ProtocolOutputGap {
                        offset: 4,
                        omitted_bytes: 6,
                        reason: ProtocolGapReason::Overflow,
                    }],
                },
            },
        );
        assert_eq!((entry.output_next, entry.output_end), (14, 40));
        assert_eq!(entry.output_chunks[0].offset, 10);
        assert_eq!(
            entry.output_chunks[0].stream,
            SessionJobOutputStreamView::Stderr
        );
        assert_eq!(entry.output_gaps[0].omitted_bytes, 6);
        let summary = entry.summary.unwrap();
        assert_eq!(summary.request_id, "req-1");
        assert_eq!(
            summary.exit,
            Some(SessionJobExitView::Signal {
                signal: "SIGSEGV".to_owned(),
                core_dumped: true
            })
        );
        assert_eq!(summary.stopped_by, Some(SessionJobStoppedByView::Kill));

        let missing = session_job_read_entry(
            handle,
            ProtocolJobReadEntry::Error {
                job_id: job_id("build"),
                error: ProtocolError::not_found("unknown job"),
            },
        );
        assert!(missing.summary.is_none());
        assert!(missing.error.is_some());
    }

    #[test]
    fn lost_jobs_are_reported_as_failed() {
        assert_eq!(
            api_record_status(EnvironmentJobStatus::Lost),
            SessionJobStatusView::Failed
        );
        assert_eq!(
            api_record_status(EnvironmentJobStatus::Queued),
            SessionJobStatusView::Queued
        );
    }

    #[test]
    fn client_refusals_keep_their_api_kind() {
        let kind = |error: ProtocolError| {
            map_environment_client_api_error(ClientError::Protocol(error)).kind
        };
        assert_eq!(
            kind(ProtocolError::invalid_params("bad")),
            AgentApiErrorKind::InvalidRequest
        );
        assert_eq!(
            kind(ProtocolError::not_found("gone")),
            AgentApiErrorKind::NotFound
        );
        assert_eq!(
            kind(ProtocolError::failed_precondition(
                environment_protocol::error::PreconditionReason::JobIdInUse,
                "in use"
            )),
            AgentApiErrorKind::Rejected
        );
        assert_eq!(
            map_environment_client_api_error(ClientError::DeadlineExceeded { elapsed_ms: 1 }).kind,
            AgentApiErrorKind::Internal
        );
    }
}
