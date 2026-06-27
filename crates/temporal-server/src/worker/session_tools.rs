use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use engine::{
    BlobRef, CoreAgentIoError, CoreAgentTools, ProviderApiKind, SessionId, ToolBatchOutcome,
    ToolBatchResumeDirective, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationResult,
    storage::{BlobStore, BlobStoreError},
};
use environments::{
    CreateJobHandle, EnvironmentId, EnvironmentRegistryError, JobHandleRecord, JobHandleStore,
    ListJobHandles, SessionEnvironmentBindingRecord, SessionEnvironmentBindingStatus,
    SessionEnvironmentBindingStore,
};
use host_client::{HostClientError, HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        jobs::{JobReadResult as HostJobReadResult, ReadJobsParams, StartJobsParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport, JobId},
};
use messaging::OutboxStore;
use serde_json::Value;
use store_pg::PgStore;
use temporal_workflow::{
    ENVIRONMENT_JOB_WAIT_DIRECTIVE_KIND, EnvironmentJobHandle, EnvironmentJobWaitDirective,
    EnvironmentJobWaitMode, EnvironmentJobWaitTerminalPolicy,
};
use tools::{
    environment::jobs::{
        JOB_CANCEL_TOOL_NAME, JOB_LIST_TOOL_NAME, JOB_READ_TOOL_NAME, JOB_START_TOOL_NAME,
        JOB_WAIT_TOOL_NAME, JobCancelArgs, JobCancelResultEntry, JobCancelResultSet, JobHandle,
        JobHandleArg, JobListArgs, JobListResultEntry, JobListResultSet, JobReadArgs,
        JobReadResultEntry, JobReadResultSet, JobStartArgs, JobStartResult, JobStarted,
        JobWaitArgs, JobWaitMode, JobWaitOutcome, JobWaitResult, JobWaitTerminalPolicy,
        is_environment_job_tool_name, visible_job_list_output, visible_job_read_output,
        wait_satisfied,
    },
    fleet::{AGENT_WAIT_TOOL_NAME, is_fleet_tool},
    fs::{FsPath, FsToolContext, MountedVfsFileSystem},
    host_protocol::RemoteHostConnection,
    limits::ToolLimits,
    messaging::{MessagingToolExecutor, is_messaging_tool},
    runtime::InlineToolRuntime,
    runtime::{ToolCatalog, ToolTarget},
    toolset::{EnvironmentToolsetConfig, ToolsetConfig, ToolsetEnvironment, resolve_toolset},
    web::fetch::WebFetchToolConfig,
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore, VfsWorkspaceStore};

use crate::{
    credential_injection::EnvironmentCredentialResolver,
    environment::{RuntimeEnvironment, SessionEnvironmentManager},
    fleet::{FleetChildRuntime, FleetService, FleetToolExecutor},
};

const DEFAULT_JOB_LIST_LIMIT: usize = 20;
const MAX_JOB_LIST_LIMIT: usize = 200;

#[derive(Clone)]
pub struct SessionTools {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mount_store: Arc<dyn VfsMountStore>,
    environments: SessionEnvironmentManager,
    environment_bindings: Option<Arc<dyn SessionEnvironmentBindingStore>>,
    environment_credentials: Option<EnvironmentCredentialResolver>,
    job_handles: Option<Arc<dyn JobHandleStore>>,
    messaging: Option<MessagingToolExecutor>,
    fleet: Option<FleetToolExecutor>,
}

impl SessionTools {
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
    ) -> Self {
        let environments = SessionEnvironmentManager::new(blobs.clone(), mount_store.clone());
        Self {
            blobs,
            workspace_store,
            mount_store,
            environments,
            environment_bindings: None,
            environment_credentials: None,
            job_handles: None,
            messaging: None,
            fleet: None,
        }
    }

    pub fn with_messaging_outbox(mut self, outbox: Arc<dyn OutboxStore>) -> Self {
        self.messaging = Some(MessagingToolExecutor::new(outbox));
        self
    }

    pub fn with_fleet_runtime(
        mut self,
        sessions: Arc<dyn engine::storage::SessionStore>,
        runtime: Arc<dyn FleetChildRuntime>,
    ) -> Self {
        let service = FleetService::new(sessions, runtime)
            .with_vfs_stores(self.workspace_store.clone(), self.mount_store.clone());
        self.fleet = Some(FleetToolExecutor::new(self.blobs.clone(), service));
        self
    }

    pub fn with_environment_bindings(
        mut self,
        bindings: Arc<dyn SessionEnvironmentBindingStore>,
    ) -> Self {
        self.environment_bindings = Some(bindings);
        self
    }

    pub(crate) fn with_environment_credentials(
        mut self,
        credentials: EnvironmentCredentialResolver,
    ) -> Self {
        self.environment_credentials = Some(credentials);
        self
    }

    pub fn with_job_handles(mut self, job_handles: Arc<dyn JobHandleStore>) -> Self {
        self.job_handles = Some(job_handles);
        self
    }

    pub fn with_environment(mut self, environment: RuntimeEnvironment) -> Self {
        self.environments.insert_environment(environment);
        self
    }

    pub fn from_pg_store(store: Arc<PgStore>) -> Self {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = store.clone();
        let mount_store: Arc<dyn VfsMountStore> = store.clone();
        let outbox: Arc<dyn OutboxStore> = store.clone();
        let environment_bindings: Arc<dyn SessionEnvironmentBindingStore> = store.clone();
        let credentials = EnvironmentCredentialResolver::from_pg_store(store.clone());
        let job_handles: Arc<dyn JobHandleStore> = store;
        Self::new(blobs, workspace_store, mount_store)
            .with_messaging_outbox(outbox)
            .with_environment_bindings(environment_bindings)
            .with_environment_credentials(credentials)
            .with_job_handles(job_handles)
    }

    pub fn from_pg_store_with_fleet_runtime(
        store: Arc<PgStore>,
        runtime: Arc<dyn FleetChildRuntime>,
    ) -> Self {
        let sessions: Arc<dyn engine::storage::SessionStore> = store.clone();
        Self::from_pg_store(store).with_fleet_runtime(sessions, runtime)
    }

    async fn invoke_messaging_call(
        &self,
        session_id: &SessionId,
        run_id: engine::RunId,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let Some(executor) = &self.messaging else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "messaging tools are not configured on this runtime",
            )
            .await;
        };
        let arguments: Value = match self.blobs.read_bytes(&call.arguments_ref).await {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        format!("invalid JSON tool arguments: {error}"),
                    )
                    .await;
                }
            },
            Err(error) => {
                return failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("read tool arguments: {error}"),
                )
                .await;
            }
        };
        match executor
            .invoke(session_id, run_id, &call.tool_name, arguments)
            .await
        {
            Ok(output) => {
                let output_bytes = serde_json::to_vec(&output.output_json)
                    .map_err(|error| io_error(format!("encode tool output: {error}")))?;
                let output_ref = self
                    .blobs
                    .put_bytes(output_bytes)
                    .await
                    .map_err(map_blob_error)?;
                let visible_ref = self
                    .blobs
                    .put_bytes(output.model_visible_text.into_bytes())
                    .await
                    .map_err(map_blob_error)?;
                Ok(ToolInvocationResult {
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(output_ref),
                    model_visible_context_entries: vec![
                        ToolInvocationResult::tool_result_context_entry(
                            &call.call_id,
                            ToolCallStatus::Succeeded,
                            visible_ref,
                        ),
                    ],
                    error_ref: None,
                    effects: output.effects,
                })
            }
            Err(error) => {
                failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string()).await
            }
        }
    }

    async fn invoke_fleet_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let Some(executor) = &self.fleet else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "Fleet tools are not configured on this runtime",
            )
            .await;
        };
        executor
            .invoke(
                crate::fleet::FleetInvocationContext {
                    parent_session_id: request.session_id.clone(),
                    parent_run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    call_id: call.call_id.clone(),
                    observed_at_ms: now_unix_ms()?,
                },
                call,
            )
            .await
    }

    async fn invoke_lone_agent_wait_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let Some(executor) = &self.fleet else {
            let call = request.calls.first().ok_or_else(|| {
                io_error("agent_wait batch had no calls after planner invocation")
            })?;
            let result = failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "Fleet tools are not configured on this runtime",
            )
            .await?;
            return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results: vec![result],
            }));
        };
        let call = request
            .calls
            .first()
            .ok_or_else(|| io_error("agent_wait batch had no calls after planner invocation"))?;
        executor
            .invoke_wait_batch(
                crate::fleet::FleetInvocationContext {
                    parent_session_id: request.session_id.clone(),
                    parent_run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    call_id: call.call_id.clone(),
                    observed_at_ms: now_unix_ms()?,
                },
                call,
            )
            .await
    }

    async fn invoke_lone_environment_job_wait_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let call = request
            .calls
            .first()
            .ok_or_else(|| io_error("job_wait batch had no calls after planner invocation"))?;
        let active_env_target = request
            .default_targets
            .get(tools::targets::ENV_TARGET_NAMESPACE);
        let environments = self
            .environment_manager_for_session(&request.session_id)
            .await?;
        let args: JobWaitArgs = self.read_tool_args(call).await?;
        if args.jobs.is_empty() {
            let result = failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "job_wait requires at least one job",
            )
            .await?;
            return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results: vec![result],
            }));
        }
        match self
            .preflight_environment_job_wait(&request, call, &environments, active_env_target, args)
            .await?
        {
            EnvironmentJobWaitPreflight::Completed(result) => {
                Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results: vec![result],
                }))
            }
            EnvironmentJobWaitPreflight::Deferred(directive) => {
                let body = serde_json::to_value(directive)
                    .map_err(|error| io_error(format!("encode job_wait directive: {error}")))?;
                Ok(ToolBatchOutcome::Deferred {
                    batch_id: request.batch_id,
                    resume_directive: ToolBatchResumeDirective::new(
                        ENVIRONMENT_JOB_WAIT_DIRECTIVE_KIND,
                        body,
                    ),
                })
            }
        }
    }

    async fn invoke_environment_job_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        environments: &SessionEnvironmentManager,
        active_env_target: Option<&engine::ToolExecutionTarget>,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_name.as_str() {
            JOB_START_TOOL_NAME => {
                let args: JobStartArgs = self.read_tool_args(call).await?;
                self.invoke_environment_job_start(
                    request,
                    call,
                    environments,
                    active_env_target,
                    args,
                )
                .await
            }
            JOB_LIST_TOOL_NAME => {
                let args: JobListArgs = self.read_tool_args(call).await?;
                let result = match self
                    .list_environment_jobs(&request.session_id, environments, args)
                    .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        return failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            error.to_string(),
                        )
                        .await;
                    }
                };
                self.succeeded_tool_result(call, &result, visible_job_list_output(&result.jobs))
                    .await
            }
            JOB_READ_TOOL_NAME => {
                let args: JobReadArgs = self.read_tool_args(call).await?;
                let result = self
                    .read_environment_jobs(
                        &request.session_id,
                        request
                            .default_targets
                            .get(tools::targets::ENV_TARGET_NAMESPACE),
                        environments,
                        args.jobs,
                        args.output_bytes,
                        args.after_seq,
                        args.include_artifacts,
                    )
                    .await?;
                self.succeeded_tool_result(
                    call,
                    &JobReadResultSet {
                        jobs: result.entries.clone(),
                    },
                    visible_job_read_output(&result.entries),
                )
                .await
            }
            JOB_WAIT_TOOL_NAME => {
                let args: JobWaitArgs = self.read_tool_args(call).await?;
                let result = self
                    .environment_job_wait_result(
                        &request.session_id,
                        active_env_target,
                        environments,
                        args,
                        false,
                    )
                    .await?;
                self.succeeded_tool_result(
                    call,
                    &result,
                    format!(
                        "job_wait outcome: {:?}\n{}",
                        result.outcome,
                        visible_job_read_output(&result.jobs)
                    ),
                )
                .await
            }
            JOB_CANCEL_TOOL_NAME => {
                let args: JobCancelArgs = self.read_tool_args(call).await?;
                let result = self
                    .cancel_environment_jobs(
                        &request.session_id,
                        active_env_target,
                        environments,
                        args,
                    )
                    .await?;
                let visible = result
                    .jobs
                    .iter()
                    .map(|entry| {
                        entry
                            .summary
                            .as_ref()
                            .map(|summary| {
                                format!("{}: {:?}", summary.job_id.as_str(), summary.status)
                            })
                            .or_else(|| {
                                entry.error.as_ref().map(|error| {
                                    let label = entry
                                        .handle
                                        .as_ref()
                                        .map(|handle| handle.job_id.as_str())
                                        .unwrap_or("<unknown>");
                                    format!("{label}: error: {error}")
                                })
                            })
                            .unwrap_or_else(|| "job_cancel: no result".to_owned())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.succeeded_tool_result(call, &result, visible).await
            }
            _ => {
                failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("unknown environment job tool {}", call.tool_name),
                )
                .await
            }
        }
    }

    async fn invoke_environment_job_start(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        environments: &SessionEnvironmentManager,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        args: JobStartArgs,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let env_id = match self.resolve_env_id(args.env_id.as_deref(), active_env_target) {
            Ok(env_id) => env_id,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error).await;
            }
        };
        let binding = match self.read_ready_binding(&request.session_id, &env_id).await {
            Ok(binding) => binding,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error).await;
            }
        };
        let Some(environment) = environments.environment(env_id.as_str()) else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                format!("environment is not reachable: {}", env_id.as_str()),
            )
            .await;
        };
        let Some(jobs) = environment.tool_context().jobs.as_ref() else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                format!(
                    "environment does not support durable jobs: {}",
                    env_id.as_str()
                ),
            )
            .await;
        };

        let params = match build_start_jobs_params(request, call, &env_id, args) {
            Ok(value) => value,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error).await;
            }
        };
        let namespace = params.namespace.clone();
        let request_hash = match start_request_hash(&params) {
            Ok(hash) => hash,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error).await;
            }
        };
        let response = match jobs.start_jobs(params).await {
            Ok(response) => response,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await;
            }
        };

        let created_at_ms = match now_unix_ms().and_then(u64_to_i64) {
            Ok(value) => value,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await;
            }
        };
        let Some(job_store) = &self.job_handles else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "job handle store is not configured on this runtime",
            )
            .await;
        };
        let handles = response
            .jobs
            .iter()
            .map(|summary| CreateJobHandle {
                session_id: request.session_id.clone(),
                env_id: env_id.clone(),
                provider_id: binding.provider_id.clone(),
                target_id: binding.target_id.clone(),
                namespace: namespace.clone(),
                job_id: summary.job_id.clone(),
                name: summary.name.clone(),
                queue_key: summary.queue_key.clone(),
                created_by_run_id: Some(request.run_id),
                created_by_turn_id: Some(request.turn_id),
                created_by_tool_call_id: Some(call.call_id.clone()),
                created_at_ms,
                start_request_hash: request_hash.clone(),
            })
            .collect::<Vec<_>>();
        let stored = match job_store.create_job_handles(handles).await {
            Ok(stored) => stored,
            Err(error) => {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await;
            }
        };
        let handle_by_job_id = stored
            .into_iter()
            .map(|record| {
                (
                    record.job_id.as_str().to_owned(),
                    handle_from_record(&record),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let result = JobStartResult {
            jobs: response
                .jobs
                .into_iter()
                .map(|summary| JobStarted {
                    name: summary.name,
                    job_id: summary.job_id.clone(),
                    handle: handle_by_job_id.get(summary.job_id.as_str()).cloned(),
                    status: summary.status,
                    dependencies: summary.dependencies,
                    queue_key: summary.queue_key,
                })
                .collect(),
        };
        let visible = result
            .jobs
            .iter()
            .map(|job| {
                let handle = job
                    .handle
                    .as_ref()
                    .map(|handle| {
                        format!("{}/{}/{}", handle.session_id, handle.env_id, handle.job_id)
                    })
                    .unwrap_or_else(|| job.job_id.to_string());
                format!("{handle}: {:?}", job.status)
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.succeeded_tool_result(call, &result, visible).await
    }

    async fn list_environment_jobs(
        &self,
        current_session_id: &SessionId,
        current_environments: &SessionEnvironmentManager,
        args: JobListArgs,
    ) -> Result<JobListResultSet, CoreAgentIoError> {
        let session_id = resolve_job_list_session_id(current_session_id, args.session_id)?;
        let env_id = args
            .env_id
            .map(EnvironmentId::try_new)
            .transpose()
            .map_err(|error| io_error(format!("invalid env_id: {error}")))?;
        let limit = normalize_job_list_limit(args.limit)?;
        let Some(job_store) = &self.job_handles else {
            return Err(io_error(
                "job handle store is not configured on this runtime",
            ));
        };
        let records = job_store
            .list_job_handles(ListJobHandles {
                session_id: session_id.clone(),
                env_id,
                limit: Some(limit),
            })
            .await
            .map_err(map_environments_error)?;

        let environments = if &session_id == current_session_id {
            current_environments.clone()
        } else {
            self.environment_manager_for_session(&session_id).await?
        };

        let mut entries = vec![None; records.len()];
        let mut grouped = BTreeMap::<EnvironmentId, Vec<(usize, JobHandleRecord)>>::new();
        for (index, record) in records.into_iter().enumerate() {
            if record.namespace != record.session_id.as_str() {
                entries[index] = Some(JobListResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    error: Some("job registry record has non-session namespace".to_owned()),
                });
                continue;
            }
            if environments.environment(record.env_id.as_str()).is_none() {
                entries[index] = Some(JobListResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    error: Some(format!("environment is not reachable: {}", record.env_id)),
                });
                continue;
            }
            grouped
                .entry(record.env_id.clone())
                .or_default()
                .push((index, record));
        }

        for (env_id, group) in grouped {
            let Some(environment) = environments.environment(env_id.as_str()) else {
                for (index, record) in group {
                    entries[index] = Some(JobListResultEntry {
                        handle: Some(handle_from_record(&record)),
                        summary: None,
                        error: Some(format!("environment is not reachable: {}", record.env_id)),
                    });
                }
                continue;
            };
            let Some(jobs) = environment.tool_context().jobs.as_ref() else {
                for (index, record) in group {
                    entries[index] = Some(JobListResultEntry {
                        handle: Some(handle_from_record(&record)),
                        summary: None,
                        error: Some(format!(
                            "environment does not support durable jobs: {}",
                            record.env_id
                        )),
                    });
                }
                continue;
            };
            let namespace = group
                .first()
                .map(|(_, record)| record.namespace.clone())
                .unwrap_or_else(|| session_id.as_str().to_owned());
            let job_ids = group
                .iter()
                .map(|(_, record)| record.job_id.clone())
                .collect::<Vec<_>>();
            match jobs
                .read_jobs(ReadJobsParams {
                    namespace,
                    jobs: job_ids,
                    after_seq: None,
                    max_bytes: None,
                    include_artifacts: false,
                    wait_ms: None,
                })
                .await
            {
                Ok(response) => {
                    let mut summaries = response.jobs.into_iter();
                    for (index, record) in group {
                        entries[index] = Some(job_list_entry_from_response(
                            handle_from_record(&record),
                            summaries.next(),
                        ));
                    }
                }
                Err(error) => {
                    for (index, record) in group {
                        entries[index] = Some(JobListResultEntry {
                            handle: Some(handle_from_record(&record)),
                            summary: None,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
        }

        Ok(JobListResultSet {
            jobs: entries
                .into_iter()
                .map(|entry| {
                    entry.unwrap_or(JobListResultEntry {
                        handle: None,
                        summary: None,
                        error: Some("job_list internal result missing".to_owned()),
                    })
                })
                .collect(),
        })
    }

    async fn preflight_environment_job_wait(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        environments: &SessionEnvironmentManager,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        args: JobWaitArgs,
    ) -> Result<EnvironmentJobWaitPreflight, CoreAgentIoError> {
        let result = self
            .environment_job_wait_result(
                &request.session_id,
                active_env_target,
                environments,
                args.clone(),
                true,
            )
            .await?;
        if result.outcome != JobWaitOutcome::Pending {
            let visible = format!(
                "job_wait outcome: {:?}\n{}",
                result.outcome,
                visible_job_read_output(&result.jobs)
            );
            let tool_result = self.succeeded_tool_result(call, &result, visible).await?;
            return Ok(EnvironmentJobWaitPreflight::Completed(tool_result));
        }
        if result.jobs.iter().any(|entry| entry.error.is_some()) {
            let visible = format!(
                "job_wait outcome: {:?}\n{}",
                result.outcome,
                visible_job_read_output(&result.jobs)
            );
            let tool_result = self.succeeded_tool_result(call, &result, visible).await?;
            return Ok(EnvironmentJobWaitPreflight::Completed(tool_result));
        }
        let handles = result
            .jobs
            .iter()
            .filter_map(|entry| entry.handle.clone())
            .map(environment_job_handle_from_tool_handle)
            .collect::<Vec<_>>();
        Ok(EnvironmentJobWaitPreflight::Deferred(
            EnvironmentJobWaitDirective {
                call_id: call.call_id.clone(),
                handles,
                mode: environment_job_wait_mode(args.mode),
                terminal_policy: environment_job_wait_terminal_policy(args.terminal_policy),
                timeout_ms: args.timeout_ms,
                output_bytes: args.output_bytes,
                include_artifacts: args.include_artifacts,
            },
        ))
    }

    async fn environment_job_wait_result(
        &self,
        session_id: &SessionId,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        environments: &SessionEnvironmentManager,
        args: JobWaitArgs,
        allow_timeout: bool,
    ) -> Result<JobWaitResult, CoreAgentIoError> {
        let read = self
            .read_environment_jobs(
                session_id,
                active_env_target,
                environments,
                args.jobs,
                args.output_bytes,
                None,
                args.include_artifacts,
            )
            .await?;
        let satisfied = wait_satisfied(&read.entries, args.mode, args.terminal_policy);
        let outcome = if satisfied {
            JobWaitOutcome::Satisfied
        } else if allow_timeout && matches!(args.timeout_ms, Some(0)) {
            JobWaitOutcome::Timeout
        } else {
            JobWaitOutcome::Pending
        };
        Ok(JobWaitResult {
            outcome,
            jobs: read.entries,
        })
    }

    async fn read_environment_jobs(
        &self,
        session_id: &SessionId,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        environments: &SessionEnvironmentManager,
        handles: Vec<JobHandleArg>,
        output_bytes: Option<usize>,
        after_seq: Option<u64>,
        include_artifacts: bool,
    ) -> Result<EnvironmentJobRead, CoreAgentIoError> {
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match self.resolve_job_handle_arg(session_id, active_env_target, handle)
            {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(JobReadResultEntry {
                        handle: None,
                        summary: None,
                        output_chunks: Vec::new(),
                        output_next_seq: 0,
                        artifacts: Vec::new(),
                        error: Some(error),
                    });
                    continue;
                }
            };
            let record = match self.read_job_handle_record(&resolved).await {
                Ok(record) => record,
                Err(error) => {
                    entries.push(JobReadResultEntry {
                        handle: Some(resolved),
                        summary: None,
                        output_chunks: Vec::new(),
                        output_next_seq: 0,
                        artifacts: Vec::new(),
                        error: Some(error),
                    });
                    continue;
                }
            };
            let Some(environment) = environments.environment(record.env_id.as_str()) else {
                entries.push(JobReadResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    output_chunks: Vec::new(),
                    output_next_seq: 0,
                    artifacts: Vec::new(),
                    error: Some(format!("environment is not reachable: {}", record.env_id)),
                });
                continue;
            };
            let Some(jobs) = environment.tool_context().jobs.as_ref() else {
                entries.push(JobReadResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    output_chunks: Vec::new(),
                    output_next_seq: 0,
                    artifacts: Vec::new(),
                    error: Some(format!(
                        "environment does not support durable jobs: {}",
                        record.env_id
                    )),
                });
                continue;
            };
            match jobs
                .read_jobs(ReadJobsParams {
                    namespace: record.namespace.clone(),
                    jobs: vec![record.job_id.clone()],
                    after_seq,
                    max_bytes: output_bytes,
                    include_artifacts,
                    wait_ms: None,
                })
                .await
            {
                Ok(response) => {
                    entries.push(job_read_entry_from_response(
                        handle_from_record(&record),
                        response.jobs.into_iter().next(),
                    ));
                }
                Err(error) => {
                    entries.push(JobReadResultEntry {
                        handle: Some(handle_from_record(&record)),
                        summary: None,
                        output_chunks: Vec::new(),
                        output_next_seq: 0,
                        artifacts: Vec::new(),
                        error: Some(error.to_string()),
                    });
                }
            }
        }
        Ok(EnvironmentJobRead { entries })
    }

    async fn cancel_environment_jobs(
        &self,
        session_id: &SessionId,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        environments: &SessionEnvironmentManager,
        args: JobCancelArgs,
    ) -> Result<JobCancelResultSet, CoreAgentIoError> {
        let mut entries = Vec::with_capacity(args.jobs.len());
        for handle in args.jobs {
            let resolved = match self.resolve_job_handle_arg(session_id, active_env_target, handle)
            {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(JobCancelResultEntry {
                        handle: None,
                        summary: None,
                        error: Some(error),
                    });
                    continue;
                }
            };
            let record = match self.read_job_handle_record(&resolved).await {
                Ok(record) => record,
                Err(error) => {
                    entries.push(JobCancelResultEntry {
                        handle: Some(resolved),
                        summary: None,
                        error: Some(error),
                    });
                    continue;
                }
            };
            let Some(environment) = environments.environment(record.env_id.as_str()) else {
                entries.push(JobCancelResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    error: Some(format!("environment is not reachable: {}", record.env_id)),
                });
                continue;
            };
            let Some(jobs) = environment.tool_context().jobs.as_ref() else {
                entries.push(JobCancelResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    error: Some(format!(
                        "environment does not support durable jobs: {}",
                        record.env_id
                    )),
                });
                continue;
            };
            match jobs
                .cancel_jobs(host_protocol::data::jobs::CancelJobsParams {
                    namespace: record.namespace.clone(),
                    jobs: vec![record.job_id.clone()],
                    scope: args.scope,
                    force: args.force,
                })
                .await
            {
                Ok(response) => {
                    let summary = response.jobs.into_iter().next();
                    entries.push(JobCancelResultEntry {
                        handle: Some(handle_from_record(&record)),
                        summary,
                        error: None,
                    });
                }
                Err(error) => entries.push(JobCancelResultEntry {
                    handle: Some(handle_from_record(&record)),
                    summary: None,
                    error: Some(error.to_string()),
                }),
            }
        }
        Ok(JobCancelResultSet { jobs: entries })
    }

    async fn read_tool_args<T>(
        &self,
        call: &engine::ToolInvocationRequest,
    ) -> Result<T, CoreAgentIoError>
    where
        T: serde::de::DeserializeOwned,
    {
        let bytes = self
            .blobs
            .read_bytes(&call.arguments_ref)
            .await
            .map_err(|error| io_error(format!("read tool arguments: {error}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io_error(format!("invalid JSON tool arguments: {error}")))
    }

    async fn succeeded_tool_result<T: serde::Serialize>(
        &self,
        call: &engine::ToolInvocationRequest,
        output: &T,
        visible: impl Into<String>,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let output_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(output).map_err(io_error)?)
            .await
            .map_err(map_blob_error)?;
        let visible_ref = self
            .blobs
            .put_bytes(visible.into().into_bytes())
            .await
            .map_err(map_blob_error)?;
        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
                &call.call_id,
                ToolCallStatus::Succeeded,
                visible_ref,
            )],
            error_ref: None,
            effects: Vec::new(),
        })
    }

    fn resolve_env_id(
        &self,
        explicit_env_id: Option<&str>,
        active_env_target: Option<&engine::ToolExecutionTarget>,
    ) -> Result<EnvironmentId, String> {
        let env_id = if let Some(env_id) = explicit_env_id {
            env_id
        } else {
            let Some(target) = active_env_target else {
                return Err("job tool requires env_id or an active environment target".to_owned());
            };
            if target.namespace != tools::targets::ENV_TARGET_NAMESPACE {
                return Err(format!(
                    "active tool target is not an environment: {}:{}",
                    target.namespace, target.id
                ));
            }
            target.id.as_str()
        };
        EnvironmentId::try_new(env_id).map_err(|error| format!("invalid env_id: {error}"))
    }

    async fn read_ready_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, String> {
        let Some(bindings) = &self.environment_bindings else {
            return Err("environment binding store is not configured on this runtime".to_owned());
        };
        let binding = bindings
            .read_binding(session_id, env_id)
            .await
            .map_err(|error| error.to_string())?;
        if binding.status != SessionEnvironmentBindingStatus::Ready {
            return Err(format!("environment is not ready: {}", env_id.as_str()));
        }
        Ok(binding)
    }

    fn resolve_job_handle_arg(
        &self,
        current_session_id: &SessionId,
        active_env_target: Option<&engine::ToolExecutionTarget>,
        handle: JobHandleArg,
    ) -> Result<JobHandle, String> {
        let session_id = match handle.session_id {
            Some(session_id) => SessionId::try_new(session_id)
                .map_err(|error| format!("invalid job handle session_id: {error}"))?,
            None => current_session_id.clone(),
        };
        if &session_id != current_session_id {
            return Err("cross-session environment job access is not supported".to_owned());
        }
        let env_id = self.resolve_env_id(handle.env_id.as_deref(), active_env_target)?;
        Ok(JobHandle {
            session_id: session_id.as_str().to_owned(),
            env_id: env_id.as_str().to_owned(),
            job_id: handle.job_id,
        })
    }

    async fn read_job_handle_record(&self, handle: &JobHandle) -> Result<JobHandleRecord, String> {
        let Some(job_store) = &self.job_handles else {
            return Err("job handle store is not configured on this runtime".to_owned());
        };
        let session_id = SessionId::try_new(handle.session_id.clone())
            .map_err(|error| format!("invalid job handle session_id: {error}"))?;
        let env_id = EnvironmentId::try_new(handle.env_id.clone())
            .map_err(|error| format!("invalid job handle env_id: {error}"))?;
        job_store
            .read_job_handle(&session_id, &env_id, &handle.job_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn environment_manager_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionEnvironmentManager, CoreAgentIoError> {
        let mut environments = self.environments.clone();
        let Some(bindings) = &self.environment_bindings else {
            return Ok(environments);
        };
        let bindings = bindings
            .list_bindings_for_session(session_id)
            .await
            .map_err(map_environments_error)?;
        for binding in bindings {
            if binding.status != SessionEnvironmentBindingStatus::Ready {
                continue;
            }
            environments.insert_environment(self.runtime_environment_for_binding(binding).await?);
        }
        Ok(environments)
    }

    async fn runtime_environment_for_binding(
        &self,
        binding: SessionEnvironmentBindingRecord,
    ) -> Result<RuntimeEnvironment, CoreAgentIoError> {
        let mut client = connect_host_data_client(&binding.connection).await?;
        let response = client
            .initialize(&InitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "lightspeed-temporal-server".to_owned(),
                scope: binding.connection.scope.clone(),
                resume_connection_id: None,
            })
            .await
            .map_err(map_host_client_error)?;
        if response.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(io_error(format!(
                "unsupported host data protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                response.protocol_version
            )));
        }
        let cwd = response
            .default_cwd
            .as_deref()
            .or_else(|| binding.cwd.as_ref().map(|cwd| cwd.as_str()))
            .map(FsPath::new)
            .transpose()
            .map_err(|error| io_error(format!("invalid host data default cwd: {error}")))?;
        client
            .initialized(&InitializedParams {})
            .await
            .map_err(map_host_client_error)?;

        let mut connection = RemoteHostConnection::new(client, response.capabilities);
        if let Some(cwd) = cwd {
            connection = connection.with_cwd(cwd);
        }
        let (fs_context, mut environment_context) = connection.into_contexts(self.blobs.clone());
        if let Some(credentials) = &self.environment_credentials {
            environment_context = credentials.wrap_context(
                environment_context,
                binding.session_id.clone(),
                binding.env_id.clone(),
            );
        }
        let environment_context =
            environment_context.with_session_id(binding.session_id.as_str().to_owned());
        crate::environment::runtime_environment_from_binding_record(&binding, environment_context)
            .map(|environment| environment.with_fs_context(fs_context))
            .map_err(io_error)
    }

    fn runtime_for_mounts(
        &self,
        mounts: Vec<VfsMountRecord>,
        environments: &SessionEnvironmentManager,
        active_env_target: Option<&engine::ToolExecutionTarget>,
    ) -> Result<InlineToolRuntime, CoreAgentIoError> {
        let catalog = workspace_catalog(
            environments.has_process_environment(),
            environments.has_job_environment(),
        )?;
        let session_fs = if mounts.is_empty() {
            None
        } else {
            let fs = MountedVfsFileSystem::new(
                self.blobs.clone(),
                self.workspace_store.clone(),
                mounts.clone(),
            )
            .map_err(io_error)?;
            let cwd = mounted_vfs_cwd(fs.mounts())?;
            Some(FsToolContext::new(Arc::new(fs), self.blobs.clone()).with_cwd(cwd))
        };
        let targets = environments
            .tool_targets(session_fs, &mounts, active_env_target)
            .map_err(io_error)?;
        Ok(InlineToolRuntime::with_targets_and_blob_store(
            targets,
            self.blobs.clone(),
            ToolLimits::default(),
            catalog,
        ))
    }
}

enum EnvironmentJobWaitPreflight {
    Completed(ToolInvocationResult),
    Deferred(EnvironmentJobWaitDirective),
}

struct EnvironmentJobRead {
    entries: Vec<JobReadResultEntry>,
}

fn build_start_jobs_params(
    request: &ToolInvocationBatchRequest,
    call: &engine::ToolInvocationRequest,
    env_id: &EnvironmentId,
    args: JobStartArgs,
) -> Result<StartJobsParams, String> {
    if args.jobs.is_empty() {
        return Err("job_start requires at least one job".to_owned());
    }
    let namespace = request.session_id.as_str().to_owned();
    let request_id = job_request_id(request, call);
    let jobs = args
        .jobs
        .into_iter()
        .enumerate()
        .map(|(index, spec)| {
            let job_id = spec
                .job_id
                .clone()
                .unwrap_or_else(|| derived_job_id(request, call, env_id, index));
            spec.into_host_spec(job_id)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StartJobsParams {
        namespace,
        request_id,
        jobs,
    })
}

fn job_request_id(
    request: &ToolInvocationBatchRequest,
    call: &engine::ToolInvocationRequest,
) -> String {
    format!(
        "jobreq:{}:{}:{}:{}",
        request.run_id.as_u64(),
        request.turn_id.as_u64(),
        request.batch_id.as_u64(),
        call.call_id.as_str()
    )
}

fn derived_job_id(
    request: &ToolInvocationBatchRequest,
    call: &engine::ToolInvocationRequest,
    env_id: &EnvironmentId,
    index: usize,
) -> JobId {
    let seed = format!(
        "{}:{}:{}:{}:{}:{}:{}",
        request.session_id,
        env_id.as_str(),
        request.run_id.as_u64(),
        request.turn_id.as_u64(),
        request.batch_id.as_u64(),
        call.call_id.as_str(),
        index
    );
    let hash = BlobRef::from_bytes(seed.as_bytes());
    let suffix = &hash.as_str()["sha256:".len().."sha256:".len() + 24];
    JobId::new(format!("job-{suffix}"))
}

fn start_request_hash(params: &StartJobsParams) -> Result<String, String> {
    serde_json::to_vec(params)
        .map(|bytes| BlobRef::from_bytes(&bytes).to_string())
        .map_err(|error| format!("encode job start request hash: {error}"))
}

fn u64_to_i64(value: u64) -> Result<i64, CoreAgentIoError> {
    i64::try_from(value).map_err(|_| io_error("current timestamp does not fit in i64 milliseconds"))
}

fn handle_from_record(record: &JobHandleRecord) -> JobHandle {
    JobHandle {
        session_id: record.session_id.as_str().to_owned(),
        env_id: record.env_id.as_str().to_owned(),
        job_id: record.job_id.clone(),
    }
}

fn environment_job_handle_from_tool_handle(handle: JobHandle) -> EnvironmentJobHandle {
    EnvironmentJobHandle {
        session_id: handle.session_id,
        env_id: handle.env_id,
        job_id: handle.job_id.as_str().to_owned(),
    }
}

fn environment_job_wait_mode(mode: JobWaitMode) -> EnvironmentJobWaitMode {
    match mode {
        JobWaitMode::All => EnvironmentJobWaitMode::All,
        JobWaitMode::Any => EnvironmentJobWaitMode::Any,
    }
}

fn environment_job_wait_terminal_policy(
    policy: JobWaitTerminalPolicy,
) -> EnvironmentJobWaitTerminalPolicy {
    match policy {
        JobWaitTerminalPolicy::AnyTerminal => EnvironmentJobWaitTerminalPolicy::AnyTerminal,
        JobWaitTerminalPolicy::AllSucceeded => EnvironmentJobWaitTerminalPolicy::AllSucceeded,
    }
}

fn job_read_entry_from_response(
    handle: JobHandle,
    response: Option<HostJobReadResult>,
) -> JobReadResultEntry {
    match response {
        Some(response) => JobReadResultEntry {
            handle: Some(handle),
            summary: Some(response.summary),
            output_chunks: response.output_chunks,
            output_next_seq: response.output_next_seq,
            artifacts: response.artifacts,
            error: None,
        },
        None => JobReadResultEntry {
            handle: Some(handle),
            summary: None,
            output_chunks: Vec::new(),
            output_next_seq: 0,
            artifacts: Vec::new(),
            error: Some("provider returned no job result".to_owned()),
        },
    }
}

fn job_list_entry_from_response(
    handle: JobHandle,
    response: Option<HostJobReadResult>,
) -> JobListResultEntry {
    match response {
        Some(response) => JobListResultEntry {
            handle: Some(handle),
            summary: Some(response.summary),
            error: None,
        },
        None => JobListResultEntry {
            handle: Some(handle),
            summary: None,
            error: Some("provider returned no job result".to_owned()),
        },
    }
}

fn resolve_job_list_session_id(
    current_session_id: &SessionId,
    explicit_session_id: Option<String>,
) -> Result<SessionId, CoreAgentIoError> {
    explicit_session_id
        .map(SessionId::try_new)
        .transpose()
        .map_err(|error| io_error(format!("invalid session_id: {error}")))?
        .map_or_else(|| Ok(current_session_id.clone()), Ok)
}

fn normalize_job_list_limit(limit: Option<usize>) -> Result<usize, CoreAgentIoError> {
    let limit = limit.unwrap_or(DEFAULT_JOB_LIST_LIMIT);
    if limit == 0 {
        return Err(io_error("job_list limit must be greater than zero"));
    }
    Ok(limit.min(MAX_JOB_LIST_LIMIT))
}

async fn connect_host_data_client(
    connection: &HostConnectionSpec,
) -> Result<HostDataClient<host_client::WebSocketTransport>, CoreAgentIoError> {
    match &connection.transport {
        HostTransport::WebSocket => HostDataClient::connect(
            &connection.endpoint,
            WebSocketConnectOptions {
                user_agent: Some("lightspeed-temporal-server".to_owned()),
                ..WebSocketConnectOptions::default()
            },
        )
        .await
        .map_err(map_host_client_error),
        HostTransport::Http => Err(unsupported_host_data_transport("http")),
        HostTransport::Stdio => Err(unsupported_host_data_transport("stdio")),
        HostTransport::Ssh => Err(unsupported_host_data_transport("ssh")),
        HostTransport::Provider { provider_type } => Err(unsupported_host_data_transport(format!(
            "provider:{provider_type}"
        ))),
    }
}

fn unsupported_host_data_transport(transport: impl std::fmt::Display) -> CoreAgentIoError {
    io_error(format!(
        "host data transport is not supported by this worker: {transport}"
    ))
}

fn has_active_environment_fs(
    environments: &SessionEnvironmentManager,
    active_env_target: Option<&engine::ToolExecutionTarget>,
) -> bool {
    let Some(target) = active_env_target else {
        return false;
    };
    target.namespace == tools::targets::ENV_TARGET_NAMESPACE
        && environments
            .environment(&target.id)
            .is_some_and(|environment| environment.fs_context().is_some())
}

fn workspace_catalog(
    include_process_tools: bool,
    include_job_tools: bool,
) -> Result<ToolCatalog, CoreAgentIoError> {
    let mut catalog = ToolCatalog::new();
    for api_kind in [
        ProviderApiKind::OpenAiResponses,
        ProviderApiKind::AnthropicMessages,
        ProviderApiKind::OpenAiCompletions,
    ] {
        let target = ToolTarget::api_kind(api_kind);
        let mut config = ToolsetConfig::workspace();
        if include_process_tools {
            config.builtin.process = EnvironmentToolsetConfig::basic();
        }
        if include_job_tools {
            config.builtin.process = config.builtin.process.with_jobs();
        }
        config.web_fetch = WebFetchToolConfig::enabled();
        let toolset = resolve_toolset(ToolsetEnvironment { target: &target }, &config)
            .map_err(|error| io_error(format!("build mounted vfs tool catalog: {error}")))?;
        for binding in toolset.catalog.bindings() {
            catalog.insert(binding.clone());
        }
    }
    Ok(catalog)
}

#[async_trait]
impl CoreAgentTools for SessionTools {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let has_agent_wait_call = request
            .calls
            .iter()
            .any(|call| call.tool_name.as_str() == AGENT_WAIT_TOOL_NAME);
        if has_agent_wait_call && request.calls.len() == 1 {
            return self.invoke_lone_agent_wait_batch(request).await;
        }
        let has_job_wait_call = request
            .calls
            .iter()
            .any(|call| call.tool_name.as_str() == JOB_WAIT_TOOL_NAME);
        if has_job_wait_call && request.calls.len() == 1 {
            return self.invoke_lone_environment_job_wait_batch(request).await;
        }
        let has_generic_runtime_call = request
            .calls
            .iter()
            .any(|call| !is_messaging_tool(&call.tool_name) && !is_fleet_tool(&call.tool_name));
        if !has_generic_runtime_call {
            // Messaging/Fleet-only batches skip generic VFS/runtime setup entirely.
            let mut results = Vec::with_capacity(request.calls.len());
            for call in &request.calls {
                if is_messaging_tool(&call.tool_name) {
                    results.push(
                        self.invoke_messaging_call(&request.session_id, request.run_id, call)
                            .await?,
                    );
                } else {
                    results.push(self.invoke_fleet_call(&request, call).await?);
                }
            }
            return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results,
            }));
        }

        let mounts = self
            .mount_store
            .list_mounts(&request.session_id)
            .await
            .map_err(map_catalog_error)?;
        let active_env_target = request
            .default_targets
            .get(tools::targets::ENV_TARGET_NAMESPACE);
        let environments = self
            .environment_manager_for_session(&request.session_id)
            .await?;
        let has_session_fs =
            !mounts.is_empty() || has_active_environment_fs(&environments, active_env_target);
        let runtime = self.runtime_for_mounts(mounts, &environments, active_env_target)?;

        let mut results = Vec::with_capacity(request.calls.len());
        for call in &request.calls {
            if is_messaging_tool(&call.tool_name) {
                results.push(
                    self.invoke_messaging_call(&request.session_id, request.run_id, call)
                        .await?,
                );
            } else if is_fleet_tool(&call.tool_name) {
                results.push(self.invoke_fleet_call(&request, call).await?);
            } else if is_environment_job_tool_name(call.tool_name.as_str()) {
                results.push(
                    self.invoke_environment_job_call(
                        &request,
                        call,
                        &environments,
                        active_env_target,
                    )
                    .await?,
                );
            } else if !has_session_fs
                && call
                    .execution_target
                    .as_ref()
                    .is_some_and(|target| target.namespace == tools::targets::FS_TARGET_NAMESPACE)
            {
                results.push(
                    failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        "session has no VFS mounts configured",
                    )
                    .await?,
                );
            } else {
                results.push(runtime.invoke_call(call).await?);
            }
        }
        Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            results,
        }))
    }
}

fn mounted_vfs_cwd(mounts: &[VfsMountRecord]) -> Result<FsPath, CoreAgentIoError> {
    let cwd = if mounts
        .iter()
        .any(|mount| mount.mount_path.as_str() == "/workspace")
    {
        "/workspace"
    } else {
        "/"
    };
    FsPath::new(cwd).map_err(io_error)
}

async fn failed_result(
    blobs: &dyn BlobStore,
    call_id: engine::ToolCallId,
    message: impl Into<String>,
) -> Result<ToolInvocationResult, CoreAgentIoError> {
    let error_ref = blobs
        .put_bytes(message.into().into_bytes())
        .await
        .map_err(map_blob_error)?;
    Ok(ToolInvocationResult {
        call_id: call_id.clone(),
        status: ToolCallStatus::Failed,
        output_ref: None,
        model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
            &call_id,
            ToolCallStatus::Failed,
            error_ref.clone(),
        )],
        error_ref: Some(error_ref),
        effects: Vec::new(),
    })
}

fn map_catalog_error(error: VfsCatalogError) -> CoreAgentIoError {
    io_error(format!("load VFS mounts: {error}"))
}

fn map_environments_error(error: EnvironmentRegistryError) -> CoreAgentIoError {
    io_error(format!("load session environment bindings: {error}"))
}

fn map_host_client_error(error: HostClientError) -> CoreAgentIoError {
    io_error(format!("host data-plane call failed: {error}"))
}

fn map_blob_error(error: BlobStoreError) -> CoreAgentIoError {
    io_error(format!("write tool error blob: {error}"))
}

fn now_unix_ms() -> Result<u64, CoreAgentIoError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| io_error(format!("system clock is before unix epoch: {error}")))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| io_error("current timestamp does not fit in u64 milliseconds"))
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use crate::environment::RuntimeEnvironment;
    use engine::{
        BlobRef, ContextEntryKind, RunId, SessionId, ToolBatchId, ToolCallId, ToolName, TurnId,
        storage::{CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
    };
    use tools::environment::{
        EnvironmentToolContext,
        process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput, ProcessRequest,
            ProcessStatus, StreamOutput, WriteProcessStdinRequest,
        },
        projection::{
            EnvironmentCapabilities, EnvironmentKind, EnvironmentRecord, EnvironmentStatus,
        },
    };
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, VfsMountAccess, VfsMountSource, VfsPath, VfsWorkspaceId, VfsWorkspaceRecord,
        create_inline_snapshot,
    };

    use super::*;

    fn visible_tool_result_ref(result: &ToolInvocationResult) -> BlobRef {
        result
            .model_visible_context_entries
            .iter()
            .find_map(|entry| {
                matches!(entry.kind, ContextEntryKind::ToolResult { .. })
                    .then(|| entry.content_ref.clone())
            })
            .expect("visible ref")
    }

    #[derive(Default)]
    struct TestCatalog {
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
        mounts: Mutex<BTreeMap<SessionId, Vec<VfsMountRecord>>>,
    }

    #[derive(Default)]
    struct RecordingProcessExecutor {
        requests: Mutex<Vec<ProcessRequest>>,
    }

    #[derive(Default)]
    struct FakeFleetRuntime {
        started_runs: Mutex<Vec<(SessionId, Vec<api::InputItem>, engine::SubmissionId)>>,
    }

    #[async_trait]
    impl FleetChildRuntime for FakeFleetRuntime {
        async fn start_session(
            &self,
            _session_id: &SessionId,
            _close_on_terminal: bool,
            _profile: Option<api::ProfileSource>,
        ) -> Result<(), api::AgentApiError> {
            Ok(())
        }

        async fn list_profiles(&self) -> Result<Vec<api::AgentProfileSummary>, api::AgentApiError> {
            Ok(Vec::new())
        }

        async fn read_profile(
            &self,
            profile_id: api::ProfileId,
        ) -> Result<api::AgentProfile, api::AgentApiError> {
            Err(api::AgentApiError::not_found(format!(
                "agent profile not found: {profile_id}"
            )))
        }

        async fn start_run(
            &self,
            session_id: &SessionId,
            input: Vec<api::InputItem>,
            submission_id: engine::SubmissionId,
        ) -> Result<String, api::AgentApiError> {
            self.started_runs.lock().expect("fleet lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }

        async fn enqueue_run(
            &self,
            session_id: &SessionId,
            input: Vec<api::InputItem>,
            submission_id: engine::SubmissionId,
        ) -> Result<String, api::AgentApiError> {
            self.started_runs.lock().expect("fleet lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }

        async fn read_session(
            &self,
            session_id: &SessionId,
        ) -> Result<api::SessionView, api::AgentApiError> {
            Ok(fleet_test_session(session_id, api::SessionStatus::Idle))
        }

        async fn read_session_events(
            &self,
            _session_id: &SessionId,
            _after: Option<u64>,
            _limit: u32,
        ) -> Result<api::SessionEventsReadResponse, api::AgentApiError> {
            Ok(api::SessionEventsReadResponse {
                events: Vec::new(),
                next_cursor: None,
                head_cursor: None,
                complete: true,
                gap: None,
            })
        }

        async fn list_session_environments(
            &self,
            _session_id: &SessionId,
        ) -> Result<api::SessionEnvironmentListResponse, api::AgentApiError> {
            Ok(api::SessionEnvironmentListResponse {
                active_env_id: None,
                environments: Vec::new(),
            })
        }

        async fn cancel_run(
            &self,
            _session_id: &SessionId,
            run_id: &str,
        ) -> Result<api::RunView, api::AgentApiError> {
            Ok(api::RunView {
                id: run_id.to_owned(),
                status: api::RunStatus::Cancelled,
                input: Vec::new(),
                items: Vec::new(),
                tool_batches: Vec::new(),
            })
        }

        async fn close_session(
            &self,
            session_id: &SessionId,
        ) -> Result<api::SessionView, api::AgentApiError> {
            Ok(fleet_test_session(session_id, api::SessionStatus::Closed))
        }
    }

    fn fleet_test_session(session_id: &SessionId, status: api::SessionStatus) -> api::SessionView {
        api::SessionView {
            id: session_id.as_str().to_owned(),
            status,
            cwd: None,
            config_revision: 0,
            config: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            runs: Vec::new(),
            active_context: api::ContextView::default(),
            active_tools: api::ActiveToolsView::default(),
            vfs_mounts: Vec::new(),
        }
    }

    #[async_trait]
    impl ProcessExecutor for RecordingProcessExecutor {
        async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            self.requests.lock().expect("process lock").push(request);
            Ok(ProcessOutput {
                status: ProcessStatus::Succeeded,
                handle: None,
                exit_code: Some(0),
                stdout: StreamOutput {
                    bytes: b"process ok".to_vec(),
                    truncated: false,
                },
                stderr: StreamOutput::default(),
            })
        }

        async fn write_stdin(
            &self,
            _request: WriteProcessStdinRequest,
        ) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not needed".to_owned(),
            })
        }
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestCatalog {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let workspace = VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            };
            self.workspaces
                .lock()
                .expect("workspace lock")
                .insert(workspace.workspace_id.clone(), workspace.clone());
            Ok(workspace)
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }

        async fn compare_and_set_head(
            &self,
            request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            let workspace = workspaces.get_mut(&request.workspace_id).ok_or_else(|| {
                VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                }
            })?;
            if let Some(expected_revision) = request.expected_revision
                && workspace.revision != expected_revision
            {
                return Err(VfsCatalogError::RevisionConflict {
                    workspace_id: request.workspace_id,
                    expected_revision,
                    actual_revision: workspace.revision,
                });
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.revision += 1;
            workspace.updated_at_ms = request.updated_at_ms;
            Ok(workspace.clone())
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .remove(workspace_id)
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }
    }

    #[async_trait]
    impl VfsMountStore for TestCatalog {
        async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
            self.mounts
                .lock()
                .expect("mount lock")
                .entry(record.session_id.clone())
                .or_default()
                .push(record);
            Ok(())
        }

        async fn list_mounts(
            &self,
            session_id: &SessionId,
        ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
            Ok(self
                .mounts
                .lock()
                .expect("mount lock")
                .get(session_id)
                .cloned()
                .unwrap_or_default())
        }

        async fn remove_mount(
            &self,
            _session_id: &SessionId,
            _mount_path: &VfsPath,
        ) -> Result<(), VfsCatalogError> {
            Ok(())
        }
    }

    async fn session_tools_with_readme_mount() -> (Arc<InMemoryBlobStore>, SessionTools, SessionId)
    {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let session_id = SessionId::new("session_1");
        let snapshot = create_inline_snapshot(
            blobs.as_ref(),
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new("README.md", b"hello\n".to_vec()).expect("inline file"),
            ]),
        )
        .await
        .expect("snapshot");
        let workspace_id = VfsWorkspaceId::new("workspace_1");
        catalog
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id: workspace_id.clone(),
                base_snapshot_ref: Some(snapshot.snapshot_ref.clone()),
                head_snapshot_ref: snapshot.snapshot_ref,
                created_at_ms: 1,
            })
            .await
            .expect("workspace");
        catalog
            .put_mount(VfsMountRecord {
                session_id: session_id.clone(),
                mount_path: VfsPath::parse("/workspace").expect("mount path"),
                source: VfsMountSource::Workspace { workspace_id },
                access: VfsMountAccess::ReadWrite,
            })
            .await
            .expect("mount");
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
        (blobs, tools, session_id)
    }

    fn test_environment(
        blobs: Arc<InMemoryBlobStore>,
        process: Arc<RecordingProcessExecutor>,
    ) -> RuntimeEnvironment {
        RuntimeEnvironment::new(
            EnvironmentRecord {
                env_id: "test".to_owned(),
                kind: EnvironmentKind::AttachedHost,
                capabilities: EnvironmentCapabilities {
                    fs_read: true,
                    fs_write: true,
                    process_exec: true,
                    process_stdin: true,
                    network: false,
                    persistent: false,
                    ..EnvironmentCapabilities::default()
                },
                exec_target: Some(tools::targets::environment_target("test")),
                cwd: Some(FsPath::new("/workspace").expect("cwd")),
                status: EnvironmentStatus::Ready,
            },
            EnvironmentToolContext::new(Some(process), blobs)
                .with_process_cwd(FsPath::new("/workspace").expect("process cwd")),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_read_session_workspace_mount() {
        let (blobs, tools, session_id) = session_tools_with_readme_mount().await;
        let arguments_ref = blobs
            .put_bytes(br#"{"path":"README.md","offset":1,"limit":10}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref,
                    execution_target: Some(tools::targets::session_fs_target()),
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let output = blobs
            .read_text(result.results[0].output_ref.as_ref().expect("output ref"))
            .await
            .expect("output");
        assert!(output.contains("hello"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_accept_claude_style_read_tool() {
        let (blobs, tools, session_id) = session_tools_with_readme_mount().await;
        let arguments_ref = blobs
            .put_bytes(br#"{"file_path":"README.md","offset":1,"limit":10}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("Read"),
                    arguments_ref,
                    execution_target: Some(tools::targets::session_fs_target()),
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let output = blobs
            .read_text(result.results[0].output_ref.as_ref().expect("output ref"))
            .await
            .expect("output");
        assert!(output.contains("hello"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_route_file_tools_to_vfs_and_process_tools_to_environment() {
        let (blobs, tools, session_id) = session_tools_with_readme_mount().await;
        let process = Arc::new(RecordingProcessExecutor::default());
        let tools = tools.with_environment(test_environment(blobs.clone(), process.clone()));
        let read_args = blobs
            .put_bytes(br#"{"path":"README.md","offset":1,"limit":10}"#.to_vec())
            .await
            .expect("read arguments");
        let process_args = blobs
            .put_bytes(br#"{"argv":["echo","hello"]}"#.to_vec())
            .await
            .expect("process arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_read"),
                        tool_name: ToolName::new("read_file"),
                        arguments_ref: read_args,
                        execution_target: Some(tools::targets::session_fs_target()),
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_process"),
                        tool_name: ToolName::new("exec_command"),
                        arguments_ref: process_args,
                        execution_target: Some(tools::targets::environment_target("test")),
                    },
                ],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results.len(), 2);
        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        if result.results[1].status != ToolCallStatus::Succeeded {
            let error = blobs
                .read_text(result.results[1].error_ref.as_ref().expect("process error"))
                .await
                .expect("process error text");
            panic!("process tool failed: {error}");
        }
        let read_output = blobs
            .read_text(result.results[0].output_ref.as_ref().expect("read output"))
            .await
            .expect("read output text");
        assert!(read_output.contains("hello"));
        let process_visible_ref = visible_tool_result_ref(&result.results[1]);
        let process_visible = blobs
            .read_text(&process_visible_ref)
            .await
            .expect("process visible text");
        assert!(process_visible.contains("process ok"));
        let requests = process.requests.lock().expect("process lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].argv,
            vec!["echo".to_owned(), "hello".to_owned()]
        );
        assert_eq!(requests[0].cwd, Some(FsPath::new("/workspace").unwrap()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn messaging_tools_enqueue_outbox_rows_without_mounts() {
        use messaging::{InMemoryOutboxStore, OutboundPayload, ReadPendingOutbound};

        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let outbox = Arc::new(InMemoryOutboxStore::new());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_messaging_outbox(outbox.clone());
        let send_args = blobs
            .put_bytes(br#"{"text":"hello from the agent","reply_to":"4123"}"#.to_vec())
            .await
            .expect("arguments");
        let noop_args = blobs
            .put_bytes(br#"{"reason":"nothing to add"}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_send"),
                        tool_name: ToolName::new("message_send"),
                        arguments_ref: send_args,
                        execution_target: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_noop"),
                        tool_name: ToolName::new("message_noop"),
                        arguments_ref: noop_args,
                        execution_target: None,
                    },
                ],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results.len(), 2);
        assert!(
            result
                .results
                .iter()
                .all(|call| call.status == ToolCallStatus::Succeeded)
        );
        let visible_ref = visible_tool_result_ref(&result.results[0]);
        let visible = blobs.read_text(&visible_ref).await.expect("visible text");
        assert!(visible.contains("Enqueued"));

        let pending = outbox
            .read_pending(ReadPendingOutbound {
                after_seq: 0,
                limit: 10,
            })
            .await
            .expect("read pending");
        assert_eq!(pending.len(), 1, "noop must not enqueue");
        assert_eq!(pending[0].run_id, Some(RunId::new(9)));
        assert_eq!(
            pending[0].payload,
            OutboundPayload::Send {
                text: "hello from the agent".to_owned(),
                reply_to: Some("4123".to_owned()),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_tools_spawn_without_generic_vfs_runtime_setup() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                agent_handle: crate::fleet::default_agent_handle(),
                created_at_ms: 1,
            })
            .await
            .expect("create parent");
        let mut state = engine::CoreAgentState::new();
        state.lifecycle.config = Some(crate::worker::default_session_config(
            engine::ModelSelection {
                api_kind: engine::ProviderApiKind::OpenAiResponses,
                provider_id: "test".to_owned(),
                model: "test-model".to_owned(),
            },
        ));
        let opening_events =
            engine::core_agent_clone_opening_events(&state, 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: parent.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("open parent");

        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let fleet_runtime = Arc::new(FakeFleetRuntime::default());
        let session_store: Arc<dyn SessionStore> = sessions;
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_fleet_runtime(session_store, fleet_runtime.clone());
        let arguments_ref = blobs
            .put_bytes(br#"{"input":"do child work"}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_spawn"),
                    tool_name: ToolName::new(::tools::fleet::AGENT_SPAWN_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        let output_ref = result.results[0].output_ref.as_ref().expect("output");
        let output: ::tools::fleet::AgentSpawnOutput =
            serde_json::from_slice(&blobs.read_bytes(output_ref).await.expect("read output"))
                .expect("decode output");
        assert!(output.child_session_id.starts_with("agent_"));
        assert_eq!(
            fleet_runtime.started_runs.lock().expect("fleet lock")[0].1,
            vec![api::InputItem::Text {
                text: "do child work".to_owned()
            }]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_wait_in_mixed_batch_fails_without_deferring() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new());
        let fleet_runtime = Arc::new(FakeFleetRuntime::default());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_fleet_runtime(sessions, fleet_runtime);
        let wait_args = blobs
            .put_bytes(br#"{"waits":[{"target_session_id":"child","run_id":"run_1"}]}"#.to_vec())
            .await
            .expect("wait args");
        let read_args = blobs
            .put_bytes(br#"{"path":"README.md"}"#.to_vec())
            .await
            .expect("read args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("parent"),
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_wait"),
                        tool_name: ToolName::new(::tools::fleet::AGENT_WAIT_TOOL_NAME),
                        arguments_ref: wait_args,
                        execution_target: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_read"),
                        tool_name: ToolName::new("read_file"),
                        arguments_ref: read_args,
                        execution_target: Some(tools::targets::session_fs_target()),
                    },
                ],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results.len(), 2);
        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        let wait_error = blobs
            .read_text(result.results[0].error_ref.as_ref().expect("wait error"))
            .await
            .expect("wait error text");
        assert!(wait_error.contains("agent_wait must be the only call"));
        assert_eq!(result.results[1].status, ToolCallStatus::Failed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_fail_host_tool_without_mounts() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
        let arguments_ref = BlobRef::from_bytes(b"{}");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("read_file"),
                    arguments_ref,
                    execution_target: Some(tools::targets::session_fs_target()),
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        let error = blobs
            .read_text(result.results[0].error_ref.as_ref().expect("error ref"))
            .await
            .expect("error");
        assert!(error.contains("no VFS mounts"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn targetless_web_fetch_runs_without_mounts() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
        let arguments_ref = blobs
            .put_bytes(br#"{"url":"http://127.0.0.1:1/","max_chars":1000}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("web_fetch"),
                    arguments_ref,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed batch");

        assert_eq!(result.results[0].status, ToolCallStatus::Failed);
        let error = blobs
            .read_text(result.results[0].error_ref.as_ref().expect("error ref"))
            .await
            .expect("error");
        assert!(error.contains("non-public"));
        assert!(!error.contains("no VFS mounts"));
    }
}
