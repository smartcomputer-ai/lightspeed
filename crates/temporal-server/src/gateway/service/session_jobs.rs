use super::*;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use engine::validate_general_string_id;
use environment_registry::{
    CreateJobHandle, EnvironmentId as RegistryEnvironmentId, JobHandleRecord, JobHandleStore,
    ListJobHandles, SessionEnvironmentBindingRecord, SessionEnvironmentBindingStatus,
    SessionEnvironmentBindingStore,
};
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
use tools::targets::ENV_TARGET_NAMESPACE;

use crate::credential_injection::EnvironmentCredentialResolver;

const DEFAULT_SESSION_JOB_LIST_LIMIT: usize = 20;
const MAX_SESSION_JOB_LIST_LIMIT: usize = 200;

impl GatewayAgentApi {
    pub(super) async fn create_session_job_records(
        &self,
        params: SessionJobCreateParams,
    ) -> Result<SessionJobCreateResponse, AgentApiError> {
        let session_id = parse_job_session_id(params.session_id)?;
        validate_job_request_id(&params.request_id)?;
        if params.jobs.is_empty() {
            return Err(AgentApiError::invalid_request(
                "session/jobs/create requires at least one job",
            ));
        }

        let loaded = self
            .load_session_state_with_current_environment_projection(&session_id)
            .await?;
        let active_env_target = loaded
            .state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE);
        let env_id = resolve_session_job_env_id(params.env_id.as_deref(), active_env_target)
            .map_err(AgentApiError::invalid_request)?;
        let binding = self.read_job_binding(&session_id, &env_id).await?;
        if binding.status != SessionEnvironmentBindingStatus::Ready {
            return Err(AgentApiError::rejected(format!(
                "environment is not ready: {}",
                env_id.as_str()
            )));
        }

        let mut jobs = Vec::with_capacity(params.jobs.len());
        for (index, spec) in params.jobs.into_iter().enumerate() {
            let job_id = match spec.job_id.as_deref() {
                Some(job_id) => {
                    parse_host_job_id(job_id).map_err(AgentApiError::invalid_request)?
                }
                None => derived_api_job_id(&session_id, &env_id, &params.request_id, index),
            };
            jobs.push(
                api_start_spec_to_host(spec, job_id).map_err(AgentApiError::invalid_request)?,
            );
        }
        let request = HostStartJobsParams {
            namespace: session_id.as_str().to_owned(),
            request_id: params.request_id,
            jobs,
        };
        let resolver = EnvironmentCredentialResolver::from_pg_store(self.store.clone());
        let mut request = request;
        for job in &mut request.jobs {
            let secret_env = resolver
                .resolve_secret_env(&session_id, &env_id, &job.env)
                .await
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            for (name, value) in secret_env {
                job.secret_env.insert(name, value);
            }
        }
        let request_hash = session_job_start_request_hash(&request)?;

        let (mut client, capabilities) = connect_initialized_host_data_client(&binding).await?;
        if !capabilities.job_start {
            return Err(AgentApiError::rejected(format!(
                "environment does not support durable job start: {}",
                env_id.as_str()
            )));
        }
        let response = client
            .start_jobs(&request)
            .await
            .map_err(map_host_client_api_error)?;

        let created_at_ms = now_ms()?;
        let records = response
            .jobs
            .iter()
            .map(|summary| CreateJobHandle {
                session_id: session_id.clone(),
                env_id: env_id.clone(),
                provider_id: binding.provider_id.clone(),
                target_id: binding.target_id.clone(),
                namespace: request.namespace.clone(),
                job_id: summary.job_id.clone(),
                name: summary.name.clone(),
                queue_key: summary.queue_key.clone(),
                created_by_run_id: None,
                created_by_turn_id: None,
                created_by_tool_call_id: None,
                created_at_ms,
                start_request_hash: request_hash.clone(),
            })
            .collect::<Vec<_>>();
        let stored = JobHandleStore::create_job_handles(self.store.as_ref(), records)
            .await
            .map_err(map_environment_registry_error)?;
        let handle_by_job_id = stored
            .iter()
            .map(|record| {
                (
                    record.job_id.as_str().to_owned(),
                    session_job_handle(record),
                )
            })
            .collect::<BTreeMap<_, _>>();

        Ok(SessionJobCreateResponse {
            env_id: env_id.as_str().to_owned(),
            jobs: response
                .jobs
                .into_iter()
                .map(|summary| SessionJobStartedView {
                    name: summary.name,
                    job_id: summary.job_id.as_str().to_owned(),
                    handle: handle_by_job_id
                        .get(summary.job_id.as_str())
                        .cloned()
                        .unwrap_or_else(|| SessionJobHandleView {
                            session_id: session_id.as_str().to_owned(),
                            env_id: env_id.as_str().to_owned(),
                            job_id: summary.job_id.as_str().to_owned(),
                        }),
                    status: api_job_status(summary.status),
                    dependencies: summary
                        .dependencies
                        .into_iter()
                        .map(|job_id| job_id.as_str().to_owned())
                        .collect(),
                    queue_key: summary.queue_key,
                })
                .collect(),
        })
    }

    pub(super) async fn list_session_job_records(
        &self,
        params: SessionJobListParams,
    ) -> Result<SessionJobListResponse, AgentApiError> {
        let session_id = parse_job_session_id(params.session_id)?;
        let env_id = params
            .env_id
            .map(|env_id| {
                RegistryEnvironmentId::try_new(env_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid env_id: {error}"))
                })
            })
            .transpose()?;
        let limit = normalize_session_job_list_limit(params.limit)?;
        let records = JobHandleStore::list_job_handles(
            self.store.as_ref(),
            ListJobHandles {
                session_id,
                env_id,
                limit: Some(limit),
            },
        )
        .await
        .map_err(map_environment_registry_error)?;
        Ok(SessionJobListResponse {
            jobs: records.iter().map(session_job_record_view).collect(),
        })
    }

    pub(super) async fn read_session_job_records(
        &self,
        params: SessionJobReadParams,
    ) -> Result<SessionJobReadResponse, AgentApiError> {
        if params.jobs.is_empty() {
            return Err(AgentApiError::invalid_request(
                "session/jobs/read requires at least one job",
            ));
        }
        let session_id = parse_job_session_id(params.session_id)?;
        let loaded = self
            .load_session_state_with_current_environment_projection(&session_id)
            .await?;
        let active_env_target = loaded
            .state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE);

        let mut entries = Vec::with_capacity(params.jobs.len());
        for handle in params.jobs {
            entries.push(
                self.read_one_session_job(
                    &session_id,
                    active_env_target,
                    handle,
                    params.output_bytes,
                    params.after_seq,
                    params.include_artifacts,
                )
                .await,
            );
        }
        Ok(SessionJobReadResponse { jobs: entries })
    }

    pub(super) async fn cancel_session_job_records(
        &self,
        params: SessionJobCancelParams,
    ) -> Result<SessionJobCancelResponse, AgentApiError> {
        if params.jobs.is_empty() {
            return Err(AgentApiError::invalid_request(
                "session/jobs/cancel requires at least one job",
            ));
        }
        let session_id = parse_job_session_id(params.session_id)?;
        let loaded = self
            .load_session_state_with_current_environment_projection(&session_id)
            .await?;
        let active_env_target = loaded
            .state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE);

        let mut entries = Vec::with_capacity(params.jobs.len());
        for handle in params.jobs {
            entries.push(
                self.cancel_one_session_job(
                    &session_id,
                    active_env_target,
                    handle,
                    params.scope,
                    params.force,
                )
                .await,
            );
        }
        Ok(SessionJobCancelResponse { jobs: entries })
    }

    async fn read_one_session_job(
        &self,
        session_id: &SessionId,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        handle: SessionJobHandleInput,
        output_bytes: Option<usize>,
        after_seq: Option<u64>,
        include_artifacts: bool,
    ) -> SessionJobReadEntryView {
        let resolved = match resolve_session_job_handle(session_id, active_env_target, handle) {
            Ok(handle) => handle,
            Err(error) => return session_job_read_error(None, error),
        };
        let record = match self.read_job_handle_record(&resolved).await {
            Ok(record) => record,
            Err(error) => return session_job_read_error(Some(resolved), error),
        };
        let mut client = match self.connect_client_for_job_record(&record, "read").await {
            Ok(client) => client,
            Err(error) => return session_job_read_error(Some(session_job_handle(&record)), error),
        };
        let response = match client
            .read_jobs(&HostReadJobsParams {
                namespace: record.namespace.clone(),
                jobs: vec![record.job_id.clone()],
                after_seq,
                max_bytes: output_bytes,
                include_artifacts,
                wait_ms: None,
            })
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return session_job_read_error(
                    Some(session_job_handle(&record)),
                    error.to_string(),
                );
            }
        };
        session_job_read_entry_from_response(
            session_job_handle(&record),
            response.jobs.into_iter().next(),
        )
    }

    async fn cancel_one_session_job(
        &self,
        session_id: &SessionId,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        handle: SessionJobHandleInput,
        scope: SessionJobCancelScopeView,
        force: bool,
    ) -> SessionJobCancelEntryView {
        let resolved = match resolve_session_job_handle(session_id, active_env_target, handle) {
            Ok(handle) => handle,
            Err(error) => return session_job_cancel_error(None, error),
        };
        let record = match self.read_job_handle_record(&resolved).await {
            Ok(record) => record,
            Err(error) => return session_job_cancel_error(Some(resolved), error),
        };
        let mut client = match self.connect_client_for_job_record(&record, "cancel").await {
            Ok(client) => client,
            Err(error) => {
                return session_job_cancel_error(Some(session_job_handle(&record)), error);
            }
        };
        let response = match client
            .cancel_jobs(&HostCancelJobsParams {
                namespace: record.namespace.clone(),
                jobs: vec![record.job_id.clone()],
                scope: host_cancel_scope(scope),
                force,
            })
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return session_job_cancel_error(
                    Some(session_job_handle(&record)),
                    error.to_string(),
                );
            }
        };
        match response.jobs.into_iter().next() {
            Some(summary) => SessionJobCancelEntryView {
                handle: Some(session_job_handle(&record)),
                summary: Some(api_job_summary(summary)),
                error: None,
            },
            None => session_job_cancel_error(
                Some(session_job_handle(&record)),
                "provider returned no job result".to_owned(),
            ),
        }
    }

    async fn read_job_handle_record(
        &self,
        handle: &SessionJobHandleView,
    ) -> Result<JobHandleRecord, String> {
        let session_id = SessionId::try_new(handle.session_id.clone())
            .map_err(|error| format!("invalid job handle session_id: {error}"))?;
        let env_id = RegistryEnvironmentId::try_new(handle.env_id.clone())
            .map_err(|error| format!("invalid job handle env_id: {error}"))?;
        let job_id = parse_host_job_id(&handle.job_id)?;
        JobHandleStore::read_job_handle(self.store.as_ref(), &session_id, &env_id, &job_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn read_job_binding(
        &self,
        session_id: &SessionId,
        env_id: &RegistryEnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, AgentApiError> {
        SessionEnvironmentBindingStore::read_binding(self.store.as_ref(), session_id, env_id)
            .await
            .map_err(map_environment_registry_error)
    }

    async fn connect_client_for_job_record(
        &self,
        record: &JobHandleRecord,
        operation: &str,
    ) -> Result<HostDataClient<host_client::WebSocketTransport>, String> {
        let binding = self
            .read_job_binding(&record.session_id, &record.env_id)
            .await
            .map_err(|error| error.to_string())?;
        if binding.provider_id != record.provider_id || binding.target_id != record.target_id {
            return Err(format!(
                "environment binding no longer points at job target: expected {}/{} got {}/{}",
                record.provider_id, record.target_id, binding.provider_id, binding.target_id
            ));
        }
        let (client, capabilities) = connect_initialized_host_data_client(&binding)
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
                record.env_id
            ));
        }
        Ok(client)
    }
}

fn parse_job_session_id(value: String) -> Result<SessionId, AgentApiError> {
    SessionId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid session id: {error}")))
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

fn resolve_session_job_env_id(
    explicit_env_id: Option<&str>,
    active_env_target: Option<&engine::ToolExecutionTarget>,
) -> Result<RegistryEnvironmentId, String> {
    let env_id = if let Some(env_id) = explicit_env_id {
        env_id
    } else {
        let Some(target) = active_env_target else {
            return Err("job API requires env_id or an active environment target".to_owned());
        };
        if target.namespace != ENV_TARGET_NAMESPACE {
            return Err(format!(
                "active tool target is not an environment: {}:{}",
                target.namespace, target.id
            ));
        }
        target.id.as_str()
    };
    RegistryEnvironmentId::try_new(env_id).map_err(|error| format!("invalid env_id: {error}"))
}

fn resolve_session_job_handle(
    current_session_id: &SessionId,
    active_env_target: Option<&engine::ToolExecutionTarget>,
    handle: SessionJobHandleInput,
) -> Result<SessionJobHandleView, String> {
    let session_id = match handle.session_id {
        Some(session_id) => SessionId::try_new(session_id)
            .map_err(|error| format!("invalid job handle session_id: {error}"))?,
        None => current_session_id.clone(),
    };
    if &session_id != current_session_id {
        return Err("cross-session environment job access is not supported".to_owned());
    }
    let env_id = resolve_session_job_env_id(handle.env_id.as_deref(), active_env_target)?;
    let job_id = parse_host_job_id(&handle.job_id)?;
    Ok(SessionJobHandleView {
        session_id: session_id.as_str().to_owned(),
        env_id: env_id.as_str().to_owned(),
        job_id: job_id.as_str().to_owned(),
    })
}

fn normalize_session_job_list_limit(limit: Option<usize>) -> Result<usize, AgentApiError> {
    let limit = limit.unwrap_or(DEFAULT_SESSION_JOB_LIST_LIMIT);
    if limit == 0 {
        return Err(AgentApiError::invalid_request(
            "session/jobs/list limit must be greater than zero",
        ));
    }
    Ok(limit.min(MAX_SESSION_JOB_LIST_LIMIT))
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
        dependency_policy: host_dependency_policy(spec.dependency_policy),
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

fn host_dependency_policy(policy: SessionJobDependencyPolicyView) -> HostJobDependencyPolicy {
    match policy {
        SessionJobDependencyPolicyView::AllSucceeded => HostJobDependencyPolicy::AllSucceeded,
        SessionJobDependencyPolicyView::AllTerminal => HostJobDependencyPolicy::AllTerminal,
    }
}

fn host_cancel_scope(scope: SessionJobCancelScopeView) -> HostJobCancelScope {
    match scope {
        SessionJobCancelScopeView::Job => HostJobCancelScope::Job,
        SessionJobCancelScopeView::Dependents => HostJobCancelScope::Dependents,
    }
}

fn derived_api_job_id(
    session_id: &SessionId,
    env_id: &RegistryEnvironmentId,
    request_id: &str,
    index: usize,
) -> JobId {
    let seed = format!(
        "{}:{}:{}:{}",
        session_id.as_str(),
        env_id.as_str(),
        request_id,
        index
    );
    let hash = BlobRef::from_bytes(seed.as_bytes());
    let suffix = &hash.as_str()["sha256:".len().."sha256:".len() + 24];
    JobId::new(format!("job-{suffix}"))
}

fn session_job_start_request_hash(request: &HostStartJobsParams) -> Result<String, AgentApiError> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SanitizedStartJobsParams<'a> {
        namespace: &'a str,
        request_id: &'a str,
        jobs: Vec<SanitizedJobStartSpec<'a>>,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SanitizedJobStartSpec<'a> {
        job_id: &'a JobId,
        name: &'a Option<String>,
        argv: &'a [String],
        cwd: &'a Option<HostPath>,
        env: &'a BTreeMap<String, String>,
        secret_env_names: Vec<&'a String>,
        stdin: &'a Option<ByteChunk>,
        timeout_ms: Option<u64>,
        depends_on: &'a [HostJobDependency],
        dependency_policy: HostJobDependencyPolicy,
        queue_key: &'a Option<String>,
    }

    let material = SanitizedStartJobsParams {
        namespace: &request.namespace,
        request_id: &request.request_id,
        jobs: request
            .jobs
            .iter()
            .map(|job| SanitizedJobStartSpec {
                job_id: &job.job_id,
                name: &job.name,
                argv: &job.argv,
                cwd: &job.cwd,
                env: &job.env,
                secret_env_names: job.secret_env.keys().collect(),
                stdin: &job.stdin,
                timeout_ms: job.timeout_ms,
                depends_on: &job.depends_on,
                dependency_policy: job.dependency_policy,
                queue_key: &job.queue_key,
            })
            .collect(),
    };
    serde_json::to_vec(&material)
        .map(|bytes| BlobRef::from_bytes(&bytes).to_string())
        .map_err(|error| AgentApiError::internal(format!("encode job start request hash: {error}")))
}

async fn connect_initialized_host_data_client(
    binding: &SessionEnvironmentBindingRecord,
) -> Result<
    (
        HostDataClient<host_client::WebSocketTransport>,
        HostCapabilities,
    ),
    AgentApiError,
> {
    let mut client = connect_host_data_client(&binding.connection).await?;
    let response = client
        .initialize(&HostInitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-temporal-server".to_owned(),
            scope: binding.connection.scope.clone(),
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

async fn connect_host_data_client(
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
            "unsupported host data transport for session jobs: {other:?}"
        ))),
    }
}

fn map_host_client_api_error(error: HostClientError) -> AgentApiError {
    match error {
        HostClientError::Host(error) => match error.code {
            HostErrorCode::InvalidRequest => AgentApiError::invalid_request(error.message),
            HostErrorCode::NotFound => AgentApiError::not_found(error.message),
            HostErrorCode::Conflict => AgentApiError::rejected(error.message),
            HostErrorCode::Unsupported | HostErrorCode::CapabilityUnavailable => {
                AgentApiError::rejected(error.message)
            }
            _ => AgentApiError::internal(error.message),
        },
        other => AgentApiError::internal(other.to_string()),
    }
}

fn session_job_record_view(record: &JobHandleRecord) -> SessionJobHandleRecordView {
    SessionJobHandleRecordView {
        handle: session_job_handle(record),
        provider_id: record.provider_id.as_str().to_owned(),
        target_id: record.target_id.as_str().to_owned(),
        namespace: record.namespace.clone(),
        name: record.name.clone(),
        queue_key: record.queue_key.clone(),
        created_by_run_id: record.created_by_run_id.map(api_run_id),
        created_by_turn_id: record.created_by_turn_id.map(|turn_id| turn_id.as_u64()),
        created_by_tool_call_id: record
            .created_by_tool_call_id
            .as_ref()
            .map(|call_id| call_id.as_str().to_owned()),
        created_at_ms: record.created_at_ms,
        start_request_hash: record.start_request_hash.clone(),
    }
}

fn session_job_handle(record: &JobHandleRecord) -> SessionJobHandleView {
    SessionJobHandleView {
        session_id: record.session_id.as_str().to_owned(),
        env_id: record.env_id.as_str().to_owned(),
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
            .map(|job_id| job_id.as_str().to_owned())
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
