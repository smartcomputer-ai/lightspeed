use super::*;

use ::environments::{
    EnvironmentInstanceId, EnvironmentInstanceRecord, EnvironmentJobGroupId, JobHandleRecord,
    JobHandleStore, ListJobHandles, ReserveEnvironmentJobGroup,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use engine::validate_general_string_id;
use host_client::{HostClientError, HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    data::{
        handshake::{
            InitializeParams as HostInitializeParams, InitializedParams as HostInitializedParams,
        },
        jobs::{
            CancelJobsParams as HostCancelJobsParams, JobArtifact as HostJobArtifact,
            JobCancelScope as HostJobCancelScope, JobDependency as HostJobDependency,
            JobDependencyPolicy as HostJobDependencyPolicy, JobOutputChunk as HostJobOutputChunk,
            JobOutputStream as HostJobOutputStream, JobReadResult as HostJobReadResult,
            JobStartSpec as HostJobStartSpec, JobStatus as HostJobStatus,
            JobSummary as HostJobSummary, ReadJobsParams as HostReadJobsParams,
            StartJobsParams as HostStartJobsParams,
        },
    },
    error::HostErrorCode,
    shared::{
        ByteChunk, CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionSpec, HostPath,
        HostTransport, JobId,
    },
};

const DEFAULT_JOB_LIST_LIMIT: usize = 20;
const MAX_JOB_LIST_LIMIT: usize = 200;

impl GatewayAgentApi {
    pub(super) async fn create_environment_job_records(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        let instance_id = parse_environment_job_instance_id(params.instance_id)?;
        self.start_environment_jobs(instance_id, params.request_id, params.jobs, None)
            .await
    }

    async fn start_environment_jobs(
        &self,
        instance_id: EnvironmentInstanceId,
        request_id: String,
        jobs: Vec<SessionJobStartSpecInput>,
        default_cwd: Option<HostPath>,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        let mut host_specs = Vec::with_capacity(jobs.len());
        for (index, spec) in jobs.into_iter().enumerate() {
            let job_id = spec
                .job_id
                .as_deref()
                .map(parse_host_job_id)
                .transpose()
                .map_err(AgentApiError::invalid_request)?
                .unwrap_or_else(|| derived_job_id(&instance_id, &request_id, index));
            let mut spec =
                api_start_spec_to_host(spec, job_id).map_err(AgentApiError::invalid_request)?;
            if spec.cwd.is_none() {
                spec.cwd = default_cwd.clone();
            }
            host_specs.push(spec);
        }
        let instance = self.read_job_instance(&instance_id).await?;
        self.start_environment_jobs_host_specs(instance, request_id, host_specs)
            .await
    }

    async fn start_environment_jobs_host_specs(
        &self,
        instance: EnvironmentInstanceRecord,
        request_id: String,
        jobs: Vec<HostJobStartSpec>,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        validate_job_request_id(&request_id)?;
        if jobs.is_empty() {
            return Err(AgentApiError::invalid_request(
                "environments/jobs/create requires at least one job",
            ));
        }
        self.read_live_environment_provider(&instance.provider_id)
            .await?;
        if !instance.capabilities.job_start {
            return Err(AgentApiError::rejected(format!(
                "environment does not support durable job start: {}",
                instance.instance_id
            )));
        }
        let request = HostStartJobsParams {
            namespace: instance.instance_id.as_str().to_owned(),
            request_id: request_id.clone(),
            jobs,
        };
        let request_hash = job_start_request_hash(&request)?;
        let job_group_id = derived_job_group_id(&instance.instance_id, &request_id);
        JobHandleStore::reserve_job_group(
            self.store.as_ref(),
            ReserveEnvironmentJobGroup {
                instance_id: instance.instance_id.clone(),
                job_group_id: job_group_id.clone(),
                request_id,
                start_request_hash: request_hash.clone(),
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;

        let job_ids = request.jobs.iter().map(|job| job.job_id.clone()).collect();
        let start_payload = temporal_workflow::EnvironmentJobStartPayload {
            request,
            start_request_hash: request_hash,
            provenance: temporal_workflow::EnvironmentJobProvenance::default(),
            credential_scope: None,
        };
        let request_ref = self
            .store
            .put_bytes(serde_json::to_vec(&start_payload).map_err(|error| {
                AgentApiError::internal(format!("encode environment job workflow start: {error}"))
            })?)
            .await
            .map_err(map_blob_store_error)?;
        let snapshot = self
            .start_environment_job_workflow(
                temporal_workflow::EnvironmentJobStartActivityRequest {
                    instance_id: instance.instance_id.as_str().to_owned(),
                    job_group_id: job_group_id.as_str().to_owned(),
                    request_ref,
                },
                job_ids,
                Vec::new(),
            )
            .await?;
        let stored = JobHandleStore::list_job_handles(
            self.store.as_ref(),
            ListJobHandles {
                instance_id: Some(instance.instance_id.clone()),
                job_group_id: Some(job_group_id.clone()),
                created_by_session_id: None,
                limit: None,
            },
        )
        .await
        .map_err(map_environments_error)?;
        let handles = stored
            .iter()
            .map(|record| {
                (
                    record.job_id.as_str().to_owned(),
                    session_job_handle(record),
                )
            })
            .collect::<BTreeMap<_, _>>();
        Ok(EnvironmentJobCreateResponse {
            instance_id: instance.instance_id.as_str().to_owned(),
            job_group_id: job_group_id.as_str().to_owned(),
            jobs: snapshot
                .jobs
                .into_iter()
                .map(|summary| SessionJobStartedView {
                    name: summary.name,
                    job_id: summary.job_id.as_str().to_owned(),
                    handle: handles
                        .get(summary.job_id.as_str())
                        .cloned()
                        .unwrap_or_else(|| SessionJobHandleView {
                            instance_id: instance.instance_id.as_str().to_owned(),
                            job_id: summary.job_id.as_str().to_owned(),
                        }),
                    promise_id: None,
                    status: api_job_status(summary.status),
                    dependencies: summary
                        .dependencies
                        .into_iter()
                        .map(|id| id.as_str().to_owned())
                        .collect(),
                    queue_key: summary.queue_key,
                })
                .collect(),
        })
    }

    async fn start_environment_job_workflow(
        &self,
        start: temporal_workflow::EnvironmentJobStartActivityRequest,
        job_ids: Vec<JobId>,
        subscriptions: Vec<temporal_workflow::EnvironmentJobSubscription>,
    ) -> Result<temporal_workflow::EnvironmentJobWorkflowSnapshot, AgentApiError> {
        let workflow_id = temporal_workflow::compose_environment_job_workflow_id(
            self.universe_id(),
            &start.instance_id,
            &start.job_group_id,
        );
        match self
            .client
            .start_workflow(
                temporal_workflow::EnvironmentJobWorkflow::run,
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
                },
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

    pub(super) async fn list_environment_job_records(
        &self,
        params: EnvironmentJobListParams,
    ) -> Result<EnvironmentJobListResponse, AgentApiError> {
        let records = JobHandleStore::list_job_handles(
            self.store.as_ref(),
            ListJobHandles {
                instance_id: params
                    .instance_id
                    .map(parse_environment_job_instance_id)
                    .transpose()?,
                job_group_id: params.job_group_id.map(parse_job_group_id).transpose()?,
                created_by_session_id: None,
                limit: Some(normalize_job_list_limit(params.limit)?),
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentJobListResponse {
            jobs: records.iter().map(session_job_record_view).collect(),
        })
    }

    pub(super) async fn read_environment_job_records(
        &self,
        params: EnvironmentJobReadParams,
    ) -> Result<EnvironmentJobReadResponse, AgentApiError> {
        let jobs = self
            .read_jobs(
                params.jobs,
                params.output_bytes,
                params.after_seq,
                params.include_artifacts,
            )
            .await?;
        Ok(EnvironmentJobReadResponse { jobs })
    }

    async fn read_jobs(
        &self,
        handles: Vec<SessionJobHandleInput>,
        output_bytes: Option<usize>,
        after_seq: Option<u64>,
        include_artifacts: bool,
    ) -> Result<Vec<SessionJobReadEntryView>, AgentApiError> {
        if handles.is_empty() {
            return Err(AgentApiError::invalid_request(
                "environments/jobs/read requires at least one job",
            ));
        }
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match parse_job_handle(handle) {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(session_job_read_error(None, error));
                    continue;
                }
            };
            let record = match self.read_job_handle_record(&resolved).await {
                Ok(record) => record,
                Err(error) => {
                    entries.push(session_job_read_error(Some(resolved), error));
                    continue;
                }
            };
            let mut client = match self.connect_client_for_job_record(&record, "read").await {
                Ok(client) => client,
                Err(error) => {
                    entries.push(session_job_read_error(Some(resolved), error));
                    continue;
                }
            };
            match client
                .read_jobs(&HostReadJobsParams {
                    namespace: record.instance_id.as_str().to_owned(),
                    jobs: vec![record.job_id.clone()],
                    after_seq,
                    max_bytes: output_bytes,
                    include_artifacts,
                    wait_ms: None,
                })
                .await
            {
                Ok(response) => entries.push(session_job_read_entry_from_response(
                    session_job_handle(&record),
                    response.jobs.into_iter().next(),
                )),
                Err(error) => entries.push(session_job_read_error(
                    Some(session_job_handle(&record)),
                    error.to_string(),
                )),
            }
        }
        Ok(entries)
    }

    pub(super) async fn cancel_environment_job_records(
        &self,
        params: EnvironmentJobCancelParams,
    ) -> Result<EnvironmentJobCancelResponse, AgentApiError> {
        let jobs = self
            .cancel_jobs(params.jobs, params.scope, params.force)
            .await?;
        Ok(EnvironmentJobCancelResponse { jobs })
    }

    async fn cancel_jobs(
        &self,
        handles: Vec<SessionJobHandleInput>,
        scope: SessionJobCancelScopeView,
        force: bool,
    ) -> Result<Vec<SessionJobCancelEntryView>, AgentApiError> {
        if handles.is_empty() {
            return Err(AgentApiError::invalid_request(
                "environments/jobs/cancel requires at least one job",
            ));
        }
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match parse_job_handle(handle) {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(session_job_cancel_error(None, error));
                    continue;
                }
            };
            let record = match self.read_job_handle_record(&resolved).await {
                Ok(record) => record,
                Err(error) => {
                    entries.push(session_job_cancel_error(Some(resolved), error));
                    continue;
                }
            };
            match self.cancel_job_via_workflow(&record, scope, force).await {
                Ok(summary) => entries.push(SessionJobCancelEntryView {
                    handle: Some(session_job_handle(&record)),
                    summary: Some(api_job_summary(summary)),
                    error: None,
                }),
                Err(error) => entries.push(session_job_cancel_error(
                    Some(session_job_handle(&record)),
                    error,
                )),
            }
        }
        Ok(entries)
    }

    async fn cancel_job_via_workflow(
        &self,
        record: &JobHandleRecord,
        scope: SessionJobCancelScopeView,
        force: bool,
    ) -> Result<HostJobSummary, String> {
        let workflow_id = temporal_workflow::compose_environment_job_workflow_id(
            self.universe_id(),
            record.instance_id.as_str(),
            record.job_group_id.as_str(),
        );
        let handle = self
            .client
            .get_workflow_handle::<temporal_workflow::EnvironmentJobWorkflow>(workflow_id);
        match handle
            .signal(
                temporal_workflow::EnvironmentJobWorkflow::cancel_jobs,
                temporal_workflow::EnvironmentJobCancelSignal {
                    jobs: vec![record.job_id.clone()],
                    scope: host_cancel_scope(scope),
                    force,
                },
                WorkflowSignalOptions::default(),
            )
            .await
        {
            Ok(()) => {
                let started = Instant::now();
                loop {
                    if started.elapsed() > self.operation_timeout {
                        return Err("timed out waiting for environment job cancellation".to_owned());
                    }
                    match handle
                        .query(
                            temporal_workflow::EnvironmentJobWorkflow::snapshot,
                            (),
                            WorkflowQueryOptions::default(),
                        )
                        .await
                    {
                        Ok(snapshot) => {
                            if let Some(summary) = snapshot.jobs.into_iter().find(|job| {
                                job.job_id == record.job_id
                                    && (job.status == HostJobStatus::CancelRequested
                                        || job.status.is_terminal())
                            }) {
                                return Ok(summary);
                            }
                        }
                        Err(WorkflowQueryError::NotFound(_) | WorkflowQueryError::Rejected(_)) => {
                            break;
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                    tokio::time::sleep(self.poll_interval).await;
                }
            }
            Err(WorkflowInteractionError::NotFound(_)) => {}
            Err(error) => return Err(error.to_string()),
        }

        let mut client = self.connect_client_for_job_record(record, "cancel").await?;
        let response = client
            .cancel_jobs(&HostCancelJobsParams {
                namespace: record.instance_id.as_str().to_owned(),
                jobs: vec![record.job_id.clone()],
                scope: host_cancel_scope(scope),
                force,
            })
            .await
            .map_err(|error| error.to_string())?;
        response
            .jobs
            .into_iter()
            .next()
            .ok_or_else(|| "provider returned no job result".to_owned())
    }

    async fn read_job_handle_record(
        &self,
        handle: &SessionJobHandleView,
    ) -> Result<JobHandleRecord, String> {
        let instance_id = EnvironmentInstanceId::try_new(handle.instance_id.clone())
            .map_err(|error| format!("invalid job handle instance_id: {error}"))?;
        let job_id = parse_host_job_id(&handle.job_id)?;
        JobHandleStore::read_job_handle(self.store.as_ref(), &instance_id, &job_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn read_job_instance(
        &self,
        instance_id: &EnvironmentInstanceId,
    ) -> Result<EnvironmentInstanceRecord, AgentApiError> {
        ::environments::EnvironmentInstanceStore::read_instance(self.store.as_ref(), instance_id)
            .await
            .map_err(map_environments_error)
    }

    async fn connect_client_for_job_record(
        &self,
        record: &JobHandleRecord,
        operation: &str,
    ) -> Result<HostDataClient<host_client::WebSocketTransport>, String> {
        let instance = self
            .read_job_instance(&record.instance_id)
            .await
            .map_err(|error| error.to_string())?;
        self.read_live_environment_provider(&instance.provider_id)
            .await
            .map_err(|error| error.to_string())?;
        let (client, capabilities) = connect_initialized_host_data_client(&instance.connection)
            .await
            .map_err(|error| error.to_string())?;
        let supported = match operation {
            "read" => capabilities.job_read,
            "cancel" => capabilities.job_cancel,
            _ => true,
        };
        if !supported {
            return Err(format!(
                "environment does not support durable job {operation}: {}",
                record.instance_id
            ));
        }
        Ok(client)
    }
}

fn parse_environment_job_instance_id(
    value: String,
) -> Result<EnvironmentInstanceId, AgentApiError> {
    EnvironmentInstanceId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid environment instance id: {error}"))
    })
}

fn parse_job_group_id(value: String) -> Result<EnvironmentJobGroupId, AgentApiError> {
    EnvironmentJobGroupId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid job group id: {error}")))
}

fn validate_job_request_id(value: &str) -> Result<(), AgentApiError> {
    validate_general_string_id("job request_id", value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid request_id: {error}")))
}

fn parse_host_job_id(value: &str) -> Result<JobId, String> {
    validate_general_string_id("job_id", value)
        .map_err(|error| format!("invalid job_id: {error}"))?;
    Ok(JobId::new(value.to_owned()))
}

fn parse_job_handle(handle: SessionJobHandleInput) -> Result<SessionJobHandleView, String> {
    let instance_id = EnvironmentInstanceId::try_new(handle.instance_id)
        .map_err(|error| format!("invalid job handle instance_id: {error}"))?;
    let job_id = parse_host_job_id(&handle.job_id)?;
    Ok(SessionJobHandleView {
        instance_id: instance_id.as_str().to_owned(),
        job_id: job_id.as_str().to_owned(),
    })
}

fn normalize_job_list_limit(limit: Option<usize>) -> Result<usize, AgentApiError> {
    let limit = limit.unwrap_or(DEFAULT_JOB_LIST_LIMIT);
    if limit == 0 {
        return Err(AgentApiError::invalid_request(
            "jobs/list limit must be greater than zero",
        ));
    }
    Ok(limit.min(MAX_JOB_LIST_LIMIT))
}

fn api_start_spec_to_host(
    spec: SessionJobStartSpecInput,
    job_id: JobId,
) -> Result<HostJobStartSpec, String> {
    if spec.argv.is_empty() {
        return Err("job argv must not be empty".to_owned());
    }
    Ok(HostJobStartSpec {
        job_id,
        name: spec.name,
        argv: spec.argv,
        cwd: spec
            .cwd
            .as_deref()
            .map(HostPath::new)
            .transpose()
            .map_err(|error| format!("invalid job cwd: {error}"))?,
        env: spec.env,
        secret_env: BTreeMap::new(),
        stdin: spec.stdin.map(|stdin| ByteChunk::from(stdin.into_bytes())),
        timeout_ms: spec.timeout_ms,
        depends_on: spec
            .depends_on
            .into_iter()
            .map(api_dependency_to_host)
            .collect::<Result<Vec<_>, _>>()?,
        dependency_policy: match spec.dependency_policy {
            SessionJobDependencyPolicyView::AllSucceeded => HostJobDependencyPolicy::AllSucceeded,
            SessionJobDependencyPolicyView::AllTerminal => HostJobDependencyPolicy::AllTerminal,
        },
        queue_key: spec.queue_key,
    })
}

fn api_dependency_to_host(
    dependency: SessionJobDependencyInput,
) -> Result<HostJobDependency, String> {
    match (dependency.job_id, dependency.name) {
        (Some(job_id), None) => Ok(HostJobDependency {
            job_id: Some(parse_host_job_id(&job_id)?),
            name: None,
        }),
        (None, Some(name)) if !name.is_empty() => Ok(HostJobDependency {
            job_id: None,
            name: Some(name),
        }),
        (Some(_), Some(_)) => {
            Err("job dependency must specify job_id or name, not both".to_owned())
        }
        _ => Err("job dependency must specify job_id or name".to_owned()),
    }
}

fn host_cancel_scope(scope: SessionJobCancelScopeView) -> HostJobCancelScope {
    match scope {
        SessionJobCancelScopeView::Job => HostJobCancelScope::Job,
        SessionJobCancelScopeView::Dependents => HostJobCancelScope::Dependents,
    }
}

fn derived_job_id(instance_id: &EnvironmentInstanceId, request_id: &str, index: usize) -> JobId {
    let hash = BlobRef::from_bytes(format!("{instance_id}:{request_id}:{index}").as_bytes());
    JobId::new(format!("job-{}", &hash.as_str()[7..31]))
}

fn derived_job_group_id(
    instance_id: &EnvironmentInstanceId,
    request_id: &str,
) -> EnvironmentJobGroupId {
    let hash = BlobRef::from_bytes(format!("{instance_id}:{request_id}").as_bytes());
    EnvironmentJobGroupId::new(format!("ejg_{}", &hash.as_str()[7..31]))
}

fn job_start_request_hash(request: &HostStartJobsParams) -> Result<String, AgentApiError> {
    serde_json::to_vec(request)
        .map(|bytes| BlobRef::from_bytes(&bytes).to_string())
        .map_err(|error| AgentApiError::internal(format!("encode job start request hash: {error}")))
}

async fn connect_initialized_host_data_client(
    connection: &HostConnectionSpec,
) -> Result<
    (
        HostDataClient<host_client::WebSocketTransport>,
        HostCapabilities,
    ),
    AgentApiError,
> {
    let mut client = connect_host_data_client(connection).await?;
    let response = client
        .initialize(&HostInitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-temporal-server".to_owned(),
            scope: connection.scope.clone(),
            resume_connection_id: None,
        })
        .await
        .map_err(map_host_client_api_error)?;
    if response.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(AgentApiError::internal(format!(
            "unsupported host data protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
            response.protocol_version
        )));
    }
    let capabilities = response.capabilities;
    client
        .initialized(&HostInitializedParams {})
        .await
        .map_err(map_host_client_api_error)?;
    Ok((client, capabilities))
}

pub(crate) async fn connect_host_data_client(
    connection: &HostConnectionSpec,
) -> Result<HostDataClient<host_client::WebSocketTransport>, AgentApiError> {
    match &connection.transport {
        HostTransport::WebSocket => HostDataClient::connect(
            &connection.endpoint,
            WebSocketConnectOptions {
                user_agent: Some("lightspeed-temporal-server".to_owned()),
                ..WebSocketConnectOptions::default()
            },
        )
        .await
        .map_err(map_host_client_api_error),
        other => Err(AgentApiError::rejected(format!(
            "unsupported host data transport for environment jobs: {other:?}"
        ))),
    }
}

fn map_host_client_api_error(error: HostClientError) -> AgentApiError {
    match error {
        HostClientError::Host(error) => match error.code {
            HostErrorCode::InvalidRequest => AgentApiError::invalid_request(error.message),
            HostErrorCode::NotFound => AgentApiError::not_found(error.message),
            HostErrorCode::Conflict
            | HostErrorCode::Unsupported
            | HostErrorCode::CapabilityUnavailable => AgentApiError::rejected(error.message),
            _ => AgentApiError::internal(error.message),
        },
        other => AgentApiError::internal(other.to_string()),
    }
}

fn session_job_record_view(record: &JobHandleRecord) -> SessionJobHandleRecordView {
    SessionJobHandleRecordView {
        handle: session_job_handle(record),
        job_group_id: record.job_group_id.as_str().to_owned(),
        name: record.name.clone(),
        queue_key: record.queue_key.clone(),
        created_by_session_id: record
            .created_by_session_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        created_by_run_id: record.created_by_run_id.map(api_run_id),
        created_by_turn_id: record.created_by_turn_id.map(|id| id.as_u64()),
        created_by_tool_call_id: record
            .created_by_tool_call_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        created_at_ms: record.created_at_ms,
        start_request_hash: record.start_request_hash.clone(),
    }
}

fn session_job_handle(record: &JobHandleRecord) -> SessionJobHandleView {
    SessionJobHandleView {
        instance_id: record.instance_id.as_str().to_owned(),
        job_id: record.job_id.as_str().to_owned(),
    }
}

fn session_job_read_entry_from_response(
    handle: SessionJobHandleView,
    response: Option<HostJobReadResult>,
) -> SessionJobReadEntryView {
    match response {
        Some(response) => SessionJobReadEntryView {
            handle: Some(handle),
            summary: Some(api_job_summary(response.summary)),
            output_chunks: response
                .output_chunks
                .into_iter()
                .map(api_job_output_chunk)
                .collect(),
            output_next_seq: response.output_next_seq,
            artifacts: response
                .artifacts
                .into_iter()
                .map(api_job_artifact)
                .collect(),
            error: None,
        },
        None => session_job_read_error(Some(handle), "provider returned no job result".to_owned()),
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
        output_next_seq: 0,
        artifacts: Vec::new(),
        error: Some(error),
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

fn api_job_summary(summary: HostJobSummary) -> SessionJobSummaryView {
    SessionJobSummaryView {
        namespace: summary.namespace,
        job_id: summary.job_id.as_str().to_owned(),
        name: summary.name,
        status: api_job_status(summary.status),
        dependencies: summary
            .dependencies
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
        created_at_ms: summary.created_at_ms,
        queued_at_ms: summary.queued_at_ms,
        started_at_ms: summary.started_at_ms,
        finished_at_ms: summary.finished_at_ms,
        exit_code: summary.exit_code,
        failure: summary.failure,
        queue_key: summary.queue_key,
    }
}

fn api_job_status(status: HostJobStatus) -> SessionJobStatusView {
    match status {
        HostJobStatus::Accepted => SessionJobStatusView::Accepted,
        HostJobStatus::Queued => SessionJobStatusView::Queued,
        HostJobStatus::Running => SessionJobStatusView::Running,
        HostJobStatus::Succeeded => SessionJobStatusView::Succeeded,
        HostJobStatus::Failed => SessionJobStatusView::Failed,
        HostJobStatus::CancelRequested => SessionJobStatusView::CancelRequested,
        HostJobStatus::Cancelled => SessionJobStatusView::Cancelled,
        HostJobStatus::TimedOut => SessionJobStatusView::TimedOut,
        HostJobStatus::DependencyFailed => SessionJobStatusView::DependencyFailed,
        HostJobStatus::Interrupted => SessionJobStatusView::Interrupted,
        HostJobStatus::Lost => SessionJobStatusView::Lost,
    }
}

fn api_job_output_chunk(chunk: HostJobOutputChunk) -> SessionJobOutputChunkView {
    SessionJobOutputChunkView {
        seq: chunk.seq,
        stream: match chunk.stream {
            HostJobOutputStream::Stdout => SessionJobOutputStreamView::Stdout,
            HostJobOutputStream::Stderr => SessionJobOutputStreamView::Stderr,
        },
        data_base64: BASE64_STANDARD.encode(chunk.chunk.into_inner()),
    }
}

fn api_job_artifact(artifact: HostJobArtifact) -> SessionJobArtifactView {
    SessionJobArtifactView {
        path: artifact.path.as_str().to_owned(),
        kind: artifact.kind,
        metadata: artifact.metadata,
    }
}
