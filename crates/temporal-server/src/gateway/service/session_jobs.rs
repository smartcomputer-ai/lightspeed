use super::*;

use ::environments::{EnvironmentId, EnvironmentJobGroupId, EnvironmentRecord};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use engine::validate_general_string_id;
use environment_client::{EnvironmentClientError, EnvironmentDataClient, WebSocketConnectOptions};
use environment_protocol::{
    data::{
        handshake::{
            InitializeParams as DataInitializeParams, InitializedParams as DataInitializedParams,
        },
        jobs::{
            CancelJobsParams as ProtocolCancelJobsParams, JobArtifact as ProtocolJobArtifact,
            JobCancelScope as ProtocolJobCancelScope, JobDependency as ProtocolJobDependency,
            JobDependencyPolicy as ProtocolJobDependencyPolicy,
            JobOutputChunk as ProtocolJobOutputChunk, JobOutputStream as ProtocolJobOutputStream,
            JobReadResult as ProtocolJobReadResult, JobStartSpec as ProtocolJobStartSpec,
            JobStatus as ProtocolJobStatus, JobSummary as ProtocolJobSummary,
            ReadJobsParams as ProtocolReadJobsParams, StartJobsParams as ProtocolStartJobsParams,
        },
    },
    error::EnvironmentProtocolErrorCode,
    shared::{
        ByteChunk, CURRENT_PROTOCOL_VERSION, EnvironmentCapabilities, EnvironmentDataConnection,
        EnvironmentPath, EnvironmentTransport, JobId,
    },
};

impl GatewayAgentApi {
    pub(super) async fn create_environment_job_records(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        let environment_id = parse_environment_job_environment_id(params.environment_id)?;
        self.start_environment_jobs(environment_id, params.request_id, params.jobs, None)
            .await
    }

    async fn start_environment_jobs(
        &self,
        environment_id: EnvironmentId,
        request_id: String,
        jobs: Vec<SessionJobStartSpecInput>,
        default_cwd: Option<EnvironmentPath>,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        let mut protocol_specs = Vec::with_capacity(jobs.len());
        for (index, spec) in jobs.into_iter().enumerate() {
            let job_id = spec
                .job_id
                .as_deref()
                .map(parse_protocol_job_id)
                .transpose()
                .map_err(AgentApiError::invalid_request)?
                .unwrap_or_else(|| derived_job_id(&environment_id, &request_id, index));
            let mut spec =
                api_start_spec_to_protocol(spec, job_id).map_err(AgentApiError::invalid_request)?;
            if spec.cwd.is_none() {
                spec.cwd = default_cwd.clone();
            }
            protocol_specs.push(spec);
        }
        let instance = self.read_job_instance(&environment_id).await?;
        self.start_environment_jobs_protocol_specs(instance, request_id, protocol_specs)
            .await
    }

    async fn start_environment_jobs_protocol_specs(
        &self,
        instance: EnvironmentRecord,
        request_id: String,
        jobs: Vec<ProtocolJobStartSpec>,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError> {
        validate_job_request_id(&request_id)?;
        if jobs.is_empty() {
            return Err(AgentApiError::invalid_request(
                "environments/jobs/create requires at least one job",
            ));
        }
        let key = self
            .environment_gateway
            .route_key(self.store.config().universe_id, &instance);
        let connection = self.environment_gateway.connection(&key);
        let (_client, capabilities) = connect_initialized_environment_data_client(
            &connection,
            self.environment_gateway
                .connect_options("lightspeed-temporal-server"),
        )
        .await
        .map_err(|error| AgentApiError::rejected(error.to_string()))?;
        if !capabilities.job_start {
            return Err(AgentApiError::rejected(format!(
                "environment does not support durable job start: {}",
                instance.environment_id
            )));
        }
        let request = ProtocolStartJobsParams {
            namespace: instance.environment_id.as_str().to_owned(),
            request_id: request_id.clone(),
            jobs,
        };
        let request_fingerprint =
            BlobRef::from_bytes(&serde_json::to_vec(&request).map_err(|error| {
                AgentApiError::internal(format!("encode environment job request: {error}"))
            })?);
        let job_group_id = derived_job_group_id(
            &instance.environment_id,
            &request_id,
            request_fingerprint.as_str(),
        );
        let job_ids = request.jobs.iter().map(|job| job.job_id.clone()).collect();
        let start_payload = temporal_workflow::EnvironmentJobStartPayload { request };
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
                    universe_id: self.universe_id(),
                    environment_id: instance.environment_id.as_str().to_owned(),
                    job_group_id: job_group_id.as_str().to_owned(),
                    request_ref,
                },
                job_ids,
                Vec::new(),
            )
            .await?;
        Ok(EnvironmentJobCreateResponse {
            environment_id: instance.environment_id.as_str().to_owned(),
            job_group_id: job_group_id.as_str().to_owned(),
            jobs: snapshot
                .jobs
                .into_iter()
                .map(|summary| SessionJobStartedView {
                    name: summary.name,
                    job_id: summary.job_id.as_str().to_owned(),
                    handle: SessionJobHandleView {
                        environment_id: instance.environment_id.as_str().to_owned(),
                        job_id: summary.job_id.as_str().to_owned(),
                    },
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
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match parse_job_handle(handle) {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(session_job_read_error(None, error));
                    continue;
                }
            };
            let environment_id = match EnvironmentId::try_new(resolved.environment_id.clone()) {
                Ok(environment_id) => environment_id,
                Err(error) => {
                    entries.push(session_job_read_error(Some(resolved), error.to_string()));
                    continue;
                }
            };
            let job_id = match parse_protocol_job_id(&resolved.job_id) {
                Ok(job_id) => job_id,
                Err(error) => {
                    entries.push(session_job_read_error(Some(resolved), error));
                    continue;
                }
            };
            let mut client = match self
                .connect_client_for_job_handle(&environment_id, "read")
                .await
            {
                Ok(client) => client,
                Err(error) => {
                    entries.push(session_job_read_error(Some(resolved), error));
                    continue;
                }
            };
            match client
                .read_jobs(&ProtocolReadJobsParams {
                    namespace: environment_id.as_str().to_owned(),
                    jobs: vec![job_id],
                    after_seq,
                    max_bytes: output_bytes,
                    include_artifacts,
                    wait_ms: None,
                })
                .await
            {
                Ok(response) => entries.push(session_job_read_entry_from_response(
                    resolved,
                    response.jobs.into_iter().next(),
                )),
                Err(error) => {
                    entries.push(session_job_read_error(Some(resolved), error.to_string()))
                }
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
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match parse_job_handle(handle) {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(session_job_cancel_error(None, error));
                    continue;
                }
            };
            let environment_id = match EnvironmentId::try_new(resolved.environment_id.clone()) {
                Ok(environment_id) => environment_id,
                Err(error) => {
                    entries.push(session_job_cancel_error(Some(resolved), error.to_string()));
                    continue;
                }
            };
            let job_id = match parse_protocol_job_id(&resolved.job_id) {
                Ok(job_id) => job_id,
                Err(error) => {
                    entries.push(session_job_cancel_error(Some(resolved), error));
                    continue;
                }
            };
            match self
                .cancel_job_on_provider(&environment_id, job_id, scope, force)
                .await
            {
                Ok(summary) => entries.push(SessionJobCancelEntryView {
                    handle: Some(resolved),
                    summary: Some(api_job_summary(summary)),
                    error: None,
                }),
                Err(error) => entries.push(session_job_cancel_error(Some(resolved), error)),
            }
        }
        Ok(entries)
    }

    async fn cancel_job_on_provider(
        &self,
        environment_id: &EnvironmentId,
        job_id: JobId,
        scope: SessionJobCancelScopeView,
        force: bool,
    ) -> Result<ProtocolJobSummary, String> {
        let mut client = self
            .connect_client_for_job_handle(environment_id, "cancel")
            .await?;
        let response = client
            .cancel_jobs(&ProtocolCancelJobsParams {
                namespace: environment_id.as_str().to_owned(),
                jobs: vec![job_id],
                scope: protocol_cancel_scope(scope),
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

    /// Resolve the environment through the shared status-aware resolver so
    /// job calls participate in wake-on-use: a powered-down
    /// provisioned environment gets its desired power set to `running` and
    /// the call fails typed `environment_not_ready` for the caller to retry
    /// — the shape polling automations lean on. Plain record reads would
    /// dial a sleeping data plane and fail generically, waking nothing.
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

    async fn connect_client_for_job_handle(
        &self,
        environment_id: &EnvironmentId,
        operation: &str,
    ) -> Result<EnvironmentDataClient<environment_client::WebSocketTransport>, String> {
        let instance = self
            .read_job_instance(environment_id)
            .await
            .map_err(|error| error.to_string())?;
        let key = self
            .environment_gateway
            .route_key(self.store.config().universe_id, &instance);
        let connection = self.environment_gateway.connection(&key);
        let (client, capabilities) = connect_initialized_environment_data_client(
            &connection,
            self.environment_gateway
                .connect_options("lightspeed-temporal-server"),
        )
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
                environment_id
            ));
        }
        Ok(client)
    }
}

fn parse_environment_job_environment_id(value: String) -> Result<EnvironmentId, AgentApiError> {
    EnvironmentId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid environment instance id: {error}"))
    })
}

fn validate_job_request_id(value: &str) -> Result<(), AgentApiError> {
    validate_general_string_id("job request_id", value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid request_id: {error}")))
}

fn parse_protocol_job_id(value: &str) -> Result<JobId, String> {
    validate_general_string_id("job_id", value)
        .map_err(|error| format!("invalid job_id: {error}"))?;
    Ok(JobId::new(value.to_owned()))
}

fn parse_job_handle(handle: SessionJobHandleInput) -> Result<SessionJobHandleView, String> {
    let environment_id = EnvironmentId::try_new(handle.environment_id)
        .map_err(|error| format!("invalid job handle environment_id: {error}"))?;
    let job_id = parse_protocol_job_id(&handle.job_id)?;
    Ok(SessionJobHandleView {
        environment_id: environment_id.as_str().to_owned(),
        job_id: job_id.as_str().to_owned(),
    })
}

fn api_start_spec_to_protocol(
    spec: SessionJobStartSpecInput,
    job_id: JobId,
) -> Result<ProtocolJobStartSpec, String> {
    if spec.argv.is_empty() {
        return Err("job argv must not be empty".to_owned());
    }
    Ok(ProtocolJobStartSpec {
        job_id,
        name: spec.name,
        argv: spec.argv,
        cwd: spec
            .cwd
            .as_deref()
            .map(EnvironmentPath::new)
            .transpose()
            .map_err(|error| format!("invalid job cwd: {error}"))?,
        env: spec.env,
        secret_env: BTreeMap::new(),
        stdin: spec.stdin.map(|stdin| ByteChunk::from(stdin.into_bytes())),
        timeout_ms: spec.timeout_ms,
        depends_on: spec
            .depends_on
            .into_iter()
            .map(api_dependency_to_protocol)
            .collect::<Result<Vec<_>, _>>()?,
        dependency_policy: match spec.dependency_policy {
            SessionJobDependencyPolicyView::AllSucceeded => {
                ProtocolJobDependencyPolicy::AllSucceeded
            }
            SessionJobDependencyPolicyView::AllTerminal => ProtocolJobDependencyPolicy::AllTerminal,
        },
        queue_key: spec.queue_key,
    })
}

fn api_dependency_to_protocol(
    dependency: SessionJobDependencyInput,
) -> Result<ProtocolJobDependency, String> {
    match (dependency.job_id, dependency.name) {
        (Some(job_id), None) => Ok(ProtocolJobDependency {
            job_id: Some(parse_protocol_job_id(&job_id)?),
            name: None,
        }),
        (None, Some(name)) if !name.is_empty() => Ok(ProtocolJobDependency {
            job_id: None,
            name: Some(name),
        }),
        (Some(_), Some(_)) => {
            Err("job dependency must specify job_id or name, not both".to_owned())
        }
        _ => Err("job dependency must specify job_id or name".to_owned()),
    }
}

fn protocol_cancel_scope(scope: SessionJobCancelScopeView) -> ProtocolJobCancelScope {
    match scope {
        SessionJobCancelScopeView::Job => ProtocolJobCancelScope::Job,
        SessionJobCancelScopeView::Dependents => ProtocolJobCancelScope::Dependents,
    }
}

fn derived_job_id(environment_id: &EnvironmentId, request_id: &str, index: usize) -> JobId {
    let hash = BlobRef::from_bytes(format!("{environment_id}:{request_id}:{index}").as_bytes());
    JobId::new(format!("job-{}", &hash.as_str()[7..31]))
}

fn derived_job_group_id(
    environment_id: &EnvironmentId,
    request_id: &str,
    request_fingerprint: &str,
) -> EnvironmentJobGroupId {
    let hash = BlobRef::from_bytes(
        format!("{environment_id}:{request_id}:{request_fingerprint}").as_bytes(),
    );
    EnvironmentJobGroupId::new(format!("ejg_{}", &hash.as_str()[7..31]))
}

async fn connect_initialized_environment_data_client(
    connection: &EnvironmentDataConnection,
    options: WebSocketConnectOptions,
) -> Result<
    (
        EnvironmentDataClient<environment_client::WebSocketTransport>,
        EnvironmentCapabilities,
    ),
    AgentApiError,
> {
    let mut client = connect_environment_data_client(connection, options).await?;
    let response = client
        .initialize(&DataInitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-temporal-server".to_owned(),
            scope: connection.scope.clone(),
            resume_connection_id: None,
        })
        .await
        .map_err(map_environment_client_api_error)?;
    if response.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(AgentApiError::internal(format!(
            "unsupported environment data protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
            response.protocol_version
        )));
    }
    let capabilities = response.capabilities;
    client
        .initialized(&DataInitializedParams {})
        .await
        .map_err(map_environment_client_api_error)?;
    Ok((client, capabilities))
}

pub(crate) async fn connect_environment_data_client(
    connection: &EnvironmentDataConnection,
    options: WebSocketConnectOptions,
) -> Result<EnvironmentDataClient<environment_client::WebSocketTransport>, AgentApiError> {
    match &connection.transport {
        EnvironmentTransport::WebSocket => {
            EnvironmentDataClient::connect(&connection.endpoint, options)
                .await
                .map_err(map_environment_client_api_error)
        }
        other => Err(AgentApiError::rejected(format!(
            "unsupported environment data transport for environment jobs: {other:?}"
        ))),
    }
}

fn map_environment_client_api_error(error: EnvironmentClientError) -> AgentApiError {
    match error {
        EnvironmentClientError::Protocol(error) => match error.code {
            EnvironmentProtocolErrorCode::InvalidRequest => {
                AgentApiError::invalid_request(error.message)
            }
            EnvironmentProtocolErrorCode::NotFound => AgentApiError::not_found(error.message),
            EnvironmentProtocolErrorCode::Conflict
            | EnvironmentProtocolErrorCode::Unsupported
            | EnvironmentProtocolErrorCode::CapabilityUnavailable => {
                AgentApiError::rejected(error.message)
            }
            _ => AgentApiError::internal(error.message),
        },
        other => AgentApiError::internal(other.to_string()),
    }
}

fn session_job_read_entry_from_response(
    handle: SessionJobHandleView,
    response: Option<ProtocolJobReadResult>,
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

fn api_job_summary(summary: ProtocolJobSummary) -> SessionJobSummaryView {
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
        orphaned_descendants: summary.orphaned_descendants,
        failure: summary.failure,
        queue_key: summary.queue_key,
    }
}

fn api_job_status(status: ProtocolJobStatus) -> SessionJobStatusView {
    match status {
        ProtocolJobStatus::Accepted => SessionJobStatusView::Accepted,
        ProtocolJobStatus::Queued => SessionJobStatusView::Queued,
        ProtocolJobStatus::Running => SessionJobStatusView::Running,
        ProtocolJobStatus::Succeeded => SessionJobStatusView::Succeeded,
        ProtocolJobStatus::Failed => SessionJobStatusView::Failed,
        ProtocolJobStatus::CancelRequested => SessionJobStatusView::CancelRequested,
        ProtocolJobStatus::Cancelled => SessionJobStatusView::Cancelled,
        ProtocolJobStatus::TimedOut => SessionJobStatusView::TimedOut,
        ProtocolJobStatus::DependencyFailed => SessionJobStatusView::DependencyFailed,
        ProtocolJobStatus::Interrupted => SessionJobStatusView::Interrupted,
        ProtocolJobStatus::Lost => SessionJobStatusView::Lost,
    }
}

fn api_job_output_chunk(chunk: ProtocolJobOutputChunk) -> SessionJobOutputChunkView {
    SessionJobOutputChunkView {
        seq: chunk.seq,
        stream: match chunk.stream {
            ProtocolJobOutputStream::Stdout => SessionJobOutputStreamView::Stdout,
            ProtocolJobOutputStream::Stderr => SessionJobOutputStreamView::Stderr,
        },
        data_base64: BASE64_STANDARD.encode(chunk.chunk.into_inner()),
    }
}

fn api_job_artifact(artifact: ProtocolJobArtifact) -> SessionJobArtifactView {
    SessionJobArtifactView {
        path: artifact.path.as_str().to_owned(),
        kind: artifact.kind,
        metadata: artifact.metadata,
    }
}
