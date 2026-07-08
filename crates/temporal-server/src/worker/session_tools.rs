use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use api_projection::{MAX_EVENT_PAGE_LIMIT, read_all_session_entries, replay_core_agent_state};
use async_trait::async_trait;
use engine::{
    BlobRef, CoreAgentIoError, CoreAgentTools, PromiseId, PromiseScope, PromiseSource,
    PromiseSourceCancelRequest, PromiseSourceCancelResult, PromiseSourceCheckRequest,
    PromiseSourceCheckResult, ProviderApiKind, SessionId, ToolBatchOutcome, ToolCallStatus,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    promise_cancel_effect, promise_create_effect, promise_detach_effect,
    storage::{BlobStore, BlobStoreError, SessionStore},
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
        jobs::{JobReadResult as HostJobReadResult, JobStatus, ReadJobsParams, StartJobsParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport, JobId},
};
use messaging::OutboxStore;
use serde_json::Value;
use store_pg::PgStore;
use tools::{
    concurrency::{
        AWAIT_TOOL_NAME, AwaitArgs, CANCEL_TOOL_NAME, CancelArgs, CancelOutput,
        CancelPromiseOutput, DETACH_TOOL_NAME, DetachArgs, DetachOutput, DetachPromiseOutput,
        SLEEP_TOOL_NAME, SleepArgs, SleepOutput, cancel_promises_model_visible_text,
        detach_promises_model_visible_text, is_concurrency_tool, sleep_model_visible_text,
    },
    environment::jobs::{
        JOB_LIST_TOOL_NAME, JOB_READ_TOOL_NAME, JOB_START_TOOL_NAME, JobCancelArgs,
        JobCancelResultEntry, JobCancelResultSet, JobHandle, JobHandleArg, JobListArgs,
        JobListResultEntry, JobListResultSet, JobReadArgs, JobReadResultEntry, JobReadResultSet,
        JobStartArgs, JobStartResult, JobStarted, is_environment_job_tool_name,
        visible_job_list_output, visible_job_read_output,
    },
    fleet::is_fleet_tool,
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
    fleet::{
        FleetChildRuntime, FleetService, FleetToolExecutor, await_spec_from_args,
        promise_status_name,
    },
};

const DEFAULT_JOB_LIST_LIMIT: usize = 20;
const MAX_JOB_LIST_LIMIT: usize = 200;
const PROMISE_JOB_OUTPUT_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct SessionTools {
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mount_store: Arc<dyn VfsMountStore>,
    sessions: Option<Arc<dyn SessionStore>>,
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
            sessions: None,
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

    pub fn with_session_store(mut self, sessions: Arc<dyn SessionStore>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    pub fn with_fleet_runtime(
        mut self,
        sessions: Arc<dyn SessionStore>,
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
        let sessions: Arc<dyn SessionStore> = store.clone();
        let outbox: Arc<dyn OutboxStore> = store.clone();
        let environment_bindings: Arc<dyn SessionEnvironmentBindingStore> = store.clone();
        let credentials = EnvironmentCredentialResolver::from_pg_store(store.clone());
        let job_handles: Arc<dyn JobHandleStore> = store;
        Self::new(blobs, workspace_store, mount_store)
            .with_session_store(sessions)
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

    async fn invoke_concurrency_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_name.as_str() {
            CANCEL_TOOL_NAME => self.invoke_store_backed_cancel_call(request, call).await,
            DETACH_TOOL_NAME => self.invoke_store_backed_detach_call(request, call).await,
            SLEEP_TOOL_NAME => self.invoke_sleep_call(request, call).await,
            AWAIT_TOOL_NAME => {
                failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    "await must be the only deferred call in its tool batch",
                )
                .await
            }
            other => {
                failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("unknown concurrency tool {other}"),
                )
                .await
            }
        }
    }

    async fn invoke_store_backed_cancel_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let result = match self
            .cancel_promises_from_session(&request.session_id, call)
            .await
        {
            Ok((output, effects)) => {
                let visible = cancel_promises_model_visible_text(&output);
                let mut result = self.succeeded_tool_result(call, &output, visible).await?;
                result.effects = effects;
                result
            }
            Err(error) => {
                failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string()).await?
            }
        };
        Ok(result)
    }

    async fn cancel_promises_from_session(
        &self,
        session_id: &SessionId,
        call: &engine::ToolInvocationRequest,
    ) -> Result<(CancelOutput, Vec<engine::ToolEffect>), CoreAgentIoError> {
        let args: CancelArgs = self.read_tool_args(call).await?;
        let promise_ids = args.validated_promise_ids().map_err(io_error)?;
        let Some(sessions) = self.sessions.as_ref() else {
            return Err(io_error("cancel requires a session store"));
        };
        let entries =
            read_all_session_entries(sessions.as_ref(), session_id, MAX_EVENT_PAGE_LIMIT as usize)
                .await
                .map_err(io_error)?;
        let state = replay_core_agent_state(&entries).map_err(io_error)?;

        let mut promises = Vec::with_capacity(promise_ids.len());
        let mut effects = Vec::new();
        for promise_id in promise_ids {
            let key = PromiseId::new(promise_id.clone());
            let Some(promise) = state.promises.promises.get(&key) else {
                return Err(io_error(format!("unknown promise {promise_id}")));
            };
            if promise.status.is_terminal() {
                promises.push(CancelPromiseOutput {
                    promise_id,
                    status: promise_status_name(promise.status).to_owned(),
                });
                continue;
            }
            effects.push(promise_cancel_effect(&key));
            promises.push(CancelPromiseOutput {
                promise_id,
                status: "cancelled".to_owned(),
            });
        }

        Ok((CancelOutput { promises }, effects))
    }

    async fn invoke_store_backed_detach_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let result = match self
            .detach_promises_from_session(&request.session_id, request.run_id, call)
            .await
        {
            Ok((output, effects)) => {
                let visible = detach_promises_model_visible_text(&output);
                let mut result = self.succeeded_tool_result(call, &output, visible).await?;
                result.effects = effects;
                result
            }
            Err(error) => {
                failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string()).await?
            }
        };
        Ok(result)
    }

    async fn detach_promises_from_session(
        &self,
        session_id: &SessionId,
        run_id: engine::RunId,
        call: &engine::ToolInvocationRequest,
    ) -> Result<(DetachOutput, Vec<engine::ToolEffect>), CoreAgentIoError> {
        let args: DetachArgs = self.read_tool_args(call).await?;
        let promise_ids = args.validated_promise_ids().map_err(io_error)?;
        let Some(sessions) = self.sessions.as_ref() else {
            return Err(io_error("detach requires a session store"));
        };
        let entries =
            read_all_session_entries(sessions.as_ref(), session_id, MAX_EVENT_PAGE_LIMIT as usize)
                .await
                .map_err(io_error)?;
        let state = replay_core_agent_state(&entries).map_err(io_error)?;

        let mut promises = Vec::with_capacity(promise_ids.len());
        let mut effects = Vec::new();
        for promise_id in promise_ids {
            let key = PromiseId::new(promise_id.clone());
            let Some(promise) = state.promises.promises.get(&key) else {
                return Err(io_error(format!("unknown promise {promise_id}")));
            };
            if promise.status.is_terminal() {
                return Err(io_error(format!(
                    "promise {promise_id} is already {}",
                    promise_status_name(promise.status)
                )));
            }
            match promise.scope {
                PromiseScope::Session => {
                    promises.push(DetachPromiseOutput {
                        promise_id,
                        status: "already_detached".to_owned(),
                    });
                }
                PromiseScope::Run {
                    run_id: promise_run_id,
                } if promise_run_id == run_id => {
                    effects.push(promise_detach_effect(&key));
                    promises.push(DetachPromiseOutput {
                        promise_id,
                        status: "detached".to_owned(),
                    });
                }
                PromiseScope::Run {
                    run_id: promise_run_id,
                } => {
                    return Err(io_error(format!(
                        "promise {promise_id} is scoped to run {promise_run_id}, not current run {run_id}",
                    )));
                }
            }
        }

        Ok((DetachOutput { promises }, effects))
    }

    async fn invoke_sleep_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: SleepArgs = self.read_tool_args(call).await?;
        let fire_at_ms = now_unix_ms()?.saturating_add(args.ms);
        let promise_id = timer_promise_id(request, call, args.ms);
        let output = SleepOutput {
            promise: promise_id.clone(),
            fire_at_ms,
        };
        let visible = sleep_model_visible_text(&output, args.ms);
        let mut result = self.succeeded_tool_result(call, &output, visible).await?;
        result.effects = vec![promise_create_effect(
            &PromiseId::new(&promise_id),
            &PromiseSource::Timer { fire_at_ms },
            None,
        )];
        Ok(result)
    }

    async fn invoke_lone_await_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let call = request
            .calls
            .first()
            .cloned()
            .ok_or_else(|| io_error("await batch had no calls after planner invocation"))?;
        if let Some(executor) = &self.fleet {
            return executor
                .invoke_await_batch(
                    crate::fleet::FleetInvocationContext {
                        parent_session_id: request.session_id.clone(),
                        parent_run_id: request.run_id,
                        turn_id: request.turn_id,
                        batch_id: request.batch_id,
                        call_id: call.call_id.clone(),
                        observed_at_ms: now_unix_ms()?,
                    },
                    &call,
                )
                .await;
        }
        self.invoke_store_backed_await_batch(request, &call).await
    }

    async fn invoke_store_backed_await_batch(
        &self,
        request: ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let context = crate::fleet::FleetInvocationContext {
            parent_session_id: request.session_id.clone(),
            parent_run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            call_id: call.call_id.clone(),
            observed_at_ms: now_unix_ms()?,
        };
        let args: AwaitArgs = self.read_tool_args(call).await?;
        match self
            .await_promises_from_session(&context, call.call_id.clone(), args)
            .await
        {
            Ok(spec) => Ok(ToolBatchOutcome::Deferred {
                batch_id: request.batch_id,
                call_id: call.call_id.clone(),
                completed_results: Vec::new(),
                spec,
            }),
            Err(error) => {
                let result =
                    failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                        .await?;
                Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results: vec![result],
                }))
            }
        }
    }

    async fn await_promises_from_session(
        &self,
        context: &crate::fleet::FleetInvocationContext,
        _call_id: engine::ToolCallId,
        args: AwaitArgs,
    ) -> Result<engine::AwaitSpec, CoreAgentIoError> {
        await_spec_from_args(args, context.observed_at_ms).map_err(io_error)
    }

    async fn invoke_mixed_await_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let await_calls = request
            .calls
            .iter()
            .filter(|call| call.tool_name.as_str() == AWAIT_TOOL_NAME)
            .cloned()
            .collect::<Vec<_>>();
        if await_calls.len() != 1 {
            let results = request
                .calls
                .iter()
                .map(|call| {
                    failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        "a tool batch may contain at most one await call",
                    )
                })
                .collect::<Vec<_>>();
            let mut completed = Vec::with_capacity(results.len());
            for result in results {
                completed.push(result.await?);
            }
            return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results: completed,
            }));
        }

        let non_await_request = ToolInvocationBatchRequest {
            calls: request
                .calls
                .iter()
                .filter(|call| call.tool_name.as_str() != AWAIT_TOOL_NAME)
                .cloned()
                .collect(),
            ..request.clone()
        };
        let completed_results = match Box::pin(self.invoke_batch(non_await_request)).await? {
            ToolBatchOutcome::Completed { result } => result.results,
            ToolBatchOutcome::Deferred { .. } => {
                let result = failed_result(
                    self.blobs.as_ref(),
                    await_calls[0].call_id.clone(),
                    "await cannot park while another call in the same batch deferred",
                )
                .await?;
                return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results: vec![result],
                }));
            }
        };

        let await_request = ToolInvocationBatchRequest {
            calls: await_calls,
            ..request.clone()
        };
        match self.invoke_lone_await_batch(await_request).await? {
            ToolBatchOutcome::Completed { result } => {
                let mut results = completed_results;
                results.extend(result.results);
                Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results,
                }))
            }
            ToolBatchOutcome::Deferred {
                batch_id,
                call_id,
                completed_results: await_completed,
                spec,
            } => {
                let mut results = completed_results;
                results.extend(await_completed);
                Ok(ToolBatchOutcome::Deferred {
                    batch_id,
                    call_id,
                    completed_results: results,
                    spec,
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
        let mut promise_effects = Vec::new();
        let result = JobStartResult {
            jobs: response
                .jobs
                .iter()
                .map(|summary| {
                    let promise_id = env_job_promise_id(request, call, &env_id, &summary.job_id);
                    promise_effects.push(promise_create_effect(
                        &PromiseId::new(&promise_id),
                        &PromiseSource::EnvJob {
                            target_session_id: request.session_id.as_str().to_owned(),
                            env_id: env_id.as_str().to_owned(),
                            job_id: summary.job_id.as_str().to_owned(),
                        },
                        None,
                    ));
                    (summary, promise_id)
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(summary, promise)| JobStarted {
                    name: summary.name.clone(),
                    job_id: summary.job_id.clone(),
                    handle: handle_by_job_id.get(summary.job_id.as_str()).cloned(),
                    status: summary.status,
                    dependencies: summary.dependencies.clone(),
                    queue_key: summary.queue_key.clone(),
                    promise: Some(promise),
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
                match job.promise.as_deref() {
                    Some(promise) => format!("{handle}: {:?} (promise {promise})", job.status),
                    None => format!("{handle}: {:?}", job.status),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut tool_result = self.succeeded_tool_result(call, &result, visible).await?;
        tool_result.effects.extend(promise_effects);
        Ok(tool_result)
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

    async fn check_env_job_promise(
        &self,
        target_session_id: String,
        env_id: String,
        job_id: String,
    ) -> Result<PromiseSourceCheckResult, CoreAgentIoError> {
        let session_id = SessionId::try_new(target_session_id)
            .map_err(|error| io_error(format!("invalid env job promise session_id: {error}")))?;
        let environments = self.environment_manager_for_session(&session_id).await?;
        let read = self
            .read_environment_jobs(
                &session_id,
                None,
                &environments,
                vec![JobHandleArg {
                    session_id: Some(session_id.as_str().to_owned()),
                    env_id: Some(env_id),
                    job_id: JobId::new(job_id),
                }],
                Some(PROMISE_JOB_OUTPUT_BYTES),
                None,
                false,
            )
            .await?;
        let Some(entry) = read.entries.into_iter().next() else {
            return self
                .blobbed_promise_failure("environment job promise read returned no entry")
                .await;
        };
        if let Some(error) = entry.error.as_ref() {
            return self.blobbed_promise_failure(error).await;
        }
        let Some(summary) = entry.summary.as_ref() else {
            return self
                .blobbed_promise_failure("environment job promise read returned no summary")
                .await;
        };
        if !summary.status.is_terminal() {
            return Ok(PromiseSourceCheckResult::Pending);
        }
        if summary.status == JobStatus::Succeeded {
            let payload_ref =
                self.blobs
                    .put_bytes(serde_json::to_vec(&entry).map_err(|error| {
                        io_error(format!("encode job promise payload: {error}"))
                    })?)
                    .await
                    .map_err(map_blob_error)?;
            return Ok(PromiseSourceCheckResult::Resolved {
                payload_ref: Some(payload_ref),
            });
        }
        let message = summary.failure.clone().unwrap_or_else(|| {
            format!(
                "environment job {} ended as {:?}",
                summary.job_id, summary.status
            )
        });
        self.blobbed_promise_failure(message).await
    }

    async fn cancel_env_job_promise(
        &self,
        target_session_id: String,
        env_id: String,
        job_id: String,
    ) -> Result<PromiseSourceCancelResult, CoreAgentIoError> {
        let session_id = SessionId::try_new(target_session_id)
            .map_err(|error| io_error(format!("invalid env job promise session_id: {error}")))?;
        let environments = self.environment_manager_for_session(&session_id).await?;
        let args = JobCancelArgs {
            jobs: vec![JobHandleArg {
                session_id: Some(session_id.as_str().to_owned()),
                env_id: Some(env_id),
                job_id: JobId::new(job_id),
            }],
            scope: Default::default(),
            force: false,
        };
        let result = self
            .cancel_environment_jobs(&session_id, None, &environments, args)
            .await?;
        Ok(PromiseSourceCancelResult {
            cancelled: result.jobs.iter().any(|entry| entry.error.is_none()),
        })
    }

    async fn blobbed_promise_failure(
        &self,
        message: impl Into<String>,
    ) -> Result<PromiseSourceCheckResult, CoreAgentIoError> {
        let error_ref = self
            .blobs
            .put_bytes(message.into().into_bytes())
            .await
            .map_err(map_blob_error)?;
        Ok(PromiseSourceCheckResult::Failed {
            error_ref: Some(error_ref),
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

fn env_job_promise_id(
    request: &ToolInvocationBatchRequest,
    call: &engine::ToolInvocationRequest,
    env_id: &EnvironmentId,
    job_id: &JobId,
) -> String {
    let seed = format!(
        "{}:{}:{}:{}:{}:{}:{}",
        request.session_id,
        env_id.as_str(),
        request.run_id.as_u64(),
        request.turn_id.as_u64(),
        request.batch_id.as_u64(),
        call.call_id.as_str(),
        job_id.as_str()
    );
    let hash = BlobRef::from_bytes(seed.as_bytes());
    let suffix = &hash.as_str()["sha256:".len().."sha256:".len() + 32];
    format!("promise_{suffix}")
}

fn timer_promise_id(
    request: &ToolInvocationBatchRequest,
    call: &engine::ToolInvocationRequest,
    ms: u64,
) -> String {
    let seed = format!(
        "{}:{}:{}:{}:{}",
        request.session_id,
        request.run_id.as_u64(),
        request.turn_id.as_u64(),
        request.batch_id.as_u64(),
        call.call_id.as_str(),
    );
    let hash = BlobRef::from_bytes(format!("{seed}:{ms}").as_bytes());
    let suffix = &hash.as_str()["sha256:".len().."sha256:".len() + 32];
    format!("promise_timer_{suffix}")
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
    async fn check_promise_source(
        &self,
        request: PromiseSourceCheckRequest,
    ) -> Result<PromiseSourceCheckResult, CoreAgentIoError> {
        match request.source {
            PromiseSource::EnvJob {
                target_session_id,
                env_id,
                job_id,
            } => {
                self.check_env_job_promise(target_session_id, env_id, job_id)
                    .await
            }
            PromiseSource::Timer { fire_at_ms } if now_unix_ms()? >= fire_at_ms => {
                Ok(PromiseSourceCheckResult::Resolved { payload_ref: None })
            }
            PromiseSource::Timer { .. } | PromiseSource::Run { .. } => {
                Ok(PromiseSourceCheckResult::Pending)
            }
        }
    }

    async fn cancel_promise_source(
        &self,
        request: PromiseSourceCancelRequest,
    ) -> Result<PromiseSourceCancelResult, CoreAgentIoError> {
        match request.source {
            PromiseSource::EnvJob {
                target_session_id,
                env_id,
                job_id,
            } => {
                self.cancel_env_job_promise(target_session_id, env_id, job_id)
                    .await
            }
            PromiseSource::Timer { .. } | PromiseSource::Run { .. } => {
                Ok(PromiseSourceCancelResult { cancelled: false })
            }
        }
    }

    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let has_await_call = request
            .calls
            .iter()
            .any(|call| call.tool_name.as_str() == AWAIT_TOOL_NAME);
        if has_await_call && request.calls.len() == 1 {
            return self.invoke_lone_await_batch(request).await;
        }
        if has_await_call {
            return self.invoke_mixed_await_batch(request).await;
        }
        let duplicate_fleet_message_call_ids =
            self.duplicate_fleet_message_call_ids(&request).await?;
        let has_generic_runtime_call = request.calls.iter().any(|call| {
            !is_messaging_tool(&call.tool_name)
                && !is_fleet_tool(&call.tool_name)
                && !is_concurrency_tool(&call.tool_name)
        });
        if !has_generic_runtime_call {
            // Messaging/Fleet/concurrency-only batches skip generic VFS/runtime setup entirely.
            let mut results = Vec::with_capacity(request.calls.len());
            for call in &request.calls {
                if duplicate_fleet_message_call_ids.contains(&call.call_id) {
                    results.push(
                        failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            "duplicate agent_send/agent_request calls with identical arguments in one tool batch are rejected",
                        )
                        .await?,
                    );
                } else if is_messaging_tool(&call.tool_name) {
                    results.push(
                        self.invoke_messaging_call(&request.session_id, request.run_id, call)
                            .await?,
                    );
                } else if is_fleet_tool(&call.tool_name) {
                    results.push(self.invoke_fleet_call(&request, call).await?);
                } else {
                    results.push(self.invoke_concurrency_call(&request, call).await?);
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
            if duplicate_fleet_message_call_ids.contains(&call.call_id) {
                results.push(
                    failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        "duplicate agent_send/agent_request calls with identical arguments in one tool batch are rejected",
                    )
                    .await?,
                );
            } else if is_messaging_tool(&call.tool_name) {
                results.push(
                    self.invoke_messaging_call(&request.session_id, request.run_id, call)
                        .await?,
                );
            } else if is_fleet_tool(&call.tool_name) {
                results.push(self.invoke_fleet_call(&request, call).await?);
            } else if is_concurrency_tool(&call.tool_name) {
                results.push(self.invoke_concurrency_call(&request, call).await?);
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

impl SessionTools {
    async fn duplicate_fleet_message_call_ids(
        &self,
        request: &ToolInvocationBatchRequest,
    ) -> Result<BTreeSet<engine::ToolCallId>, CoreAgentIoError> {
        let mut by_arguments = BTreeMap::<Vec<u8>, Vec<engine::ToolCallId>>::new();
        for call in &request.calls {
            if !matches!(
                call.tool_name.as_str(),
                tools::fleet::AGENT_SEND_TOOL_NAME | tools::fleet::AGENT_REQUEST_TOOL_NAME
            ) {
                continue;
            }
            let bytes = self
                .blobs
                .read_bytes(&call.arguments_ref)
                .await
                .map_err(map_blob_error)?;
            by_arguments
                .entry(bytes)
                .or_default()
                .push(call.call_id.clone());
        }
        Ok(by_arguments
            .into_values()
            .filter(|call_ids| call_ids.len() > 1)
            .flatten()
            .collect())
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
            _notify_on_terminal: Vec<engine::RunTerminalNotifyIntent>,
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
            _notify_on_terminal: Vec<engine::RunTerminalNotifyIntent>,
        ) -> Result<String, api::AgentApiError> {
            self.started_runs.lock().expect("fleet lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }

        async fn deliver_message(
            &self,
            session_id: &SessionId,
            input: Vec<api::InputItem>,
            submission_id: engine::SubmissionId,
        ) -> Result<(), api::AgentApiError> {
            self.started_runs.lock().expect("fleet lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok(())
        }

        async fn holder_workflow_id(
            &self,
            session_id: &SessionId,
        ) -> Result<String, api::AgentApiError> {
            Ok(format!("test-universe/{session_id}"))
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
    }

    fn fleet_test_session(session_id: &SessionId, status: api::SessionStatus) -> api::SessionView {
        api::SessionView {
            id: session_id.as_str().to_owned(),
            status,
            display_name: None,
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
                display_name: record.display_name,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                head_totals: record.head_totals,
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

        async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
            Ok(self
                .workspaces
                .lock()
                .expect("workspace lock")
                .values()
                .cloned()
                .collect())
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
            if let Some(display_name) = request.display_name {
                workspace.display_name = Some(display_name);
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.head_totals = request.new_head_totals;
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
                display_name: None,
                base_snapshot_ref: Some(snapshot.snapshot_ref.clone()),
                head_snapshot_ref: snapshot.snapshot_ref,
                head_totals: snapshot.manifest.totals.clone(),
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
                display_name: None,
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
    async fn duplicate_agent_send_calls_in_one_batch_are_rejected() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                display_name: None,
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
            .put_bytes(br#"{"to":{"kind":"parent"},"text":"same"}"#.to_vec())
            .await
            .expect("arguments");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_send_1"),
                        tool_name: ToolName::new(::tools::fleet::AGENT_SEND_TOOL_NAME),
                        arguments_ref: arguments_ref.clone(),
                        execution_target: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_send_2"),
                        tool_name: ToolName::new(::tools::fleet::AGENT_SEND_TOOL_NAME),
                        arguments_ref,
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
                .all(|result| result.status == ToolCallStatus::Failed)
        );
        assert!(
            fleet_runtime
                .started_runs
                .lock()
                .expect("fleet lock")
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_in_mixed_batch_defers_with_completed_non_await_results() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                display_name: None,
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
        let mut opening_events =
            engine::core_agent_clone_opening_events(&state, 2).expect("opening events");
        opening_events.push(
            engine::CoreAgentCodec
                .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                    observed_at_ms: 3,
                    joins: Default::default(),
                    event: engine::CoreAgentEvent::Promise(engine::PromiseEvent::Created {
                        promise: engine::Promise {
                            promise_id: engine::PromiseId::new("promise_child"),
                            source: engine::PromiseSource::Timer { fire_at_ms: 60_000 },
                            scope: engine::PromiseScope::Session,
                            status: engine::PromiseStatus::Pending,
                            payload_ref: None,
                            error_ref: None,
                            deadline_ms: None,
                        },
                    }),
                })
                .expect("encode promise"),
        );
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: parent.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("open parent with promise");
        let fleet_runtime = Arc::new(FakeFleetRuntime::default());
        let session_store: Arc<dyn SessionStore> = sessions;
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_fleet_runtime(session_store, fleet_runtime);
        let wait_args = blobs
            .put_bytes(br#"{"promises":["promise_child"]}"#.to_vec())
            .await
            .expect("await args");
        let read_args = blobs
            .put_bytes(br#"{"path":"README.md"}"#.to_vec())
            .await
            .expect("read args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_wait"),
                        tool_name: ToolName::new(::tools::concurrency::AWAIT_TOOL_NAME),
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
            .expect("invoke");

        let ToolBatchOutcome::Deferred {
            batch_id,
            call_id,
            completed_results,
            spec,
        } = result
        else {
            panic!("expected deferred mixed await batch");
        };
        assert_eq!(batch_id, ToolBatchId::new(1));
        assert_eq!(call_id, ToolCallId::new("call_wait"));
        assert_eq!(spec.promise_ids, vec![PromiseId::new("promise_child")]);
        assert_eq!(completed_results.len(), 1);
        assert_eq!(completed_results[0].call_id, ToolCallId::new("call_read"));
        assert_eq!(completed_results[0].status, ToolCallStatus::Failed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_defers_without_fleet_runtime() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent_no_fleet_await");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                display_name: None,
                created_at_ms: 1,
            })
            .await
            .expect("create parent");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: parent.clone(),
                expected_head: None,
                events: vec![
                    engine::CoreAgentCodec
                        .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                            observed_at_ms: 3,
                            joins: Default::default(),
                            event: engine::CoreAgentEvent::Promise(engine::PromiseEvent::Created {
                                promise: engine::Promise {
                                    promise_id: engine::PromiseId::new("promise_job"),
                                    source: engine::PromiseSource::Timer { fire_at_ms: 60_000 },
                                    scope: engine::PromiseScope::Session,
                                    status: engine::PromiseStatus::Pending,
                                    payload_ref: None,
                                    error_ref: None,
                                    deadline_ms: None,
                                },
                            }),
                        })
                        .expect("encode promise"),
                ],
            })
            .await
            .expect("append promise");
        let session_store: Arc<dyn SessionStore> = sessions;
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_session_store(session_store);
        let wait_args = blobs
            .put_bytes(br#"{"promises":["promise_job"]}"#.to_vec())
            .await
            .expect("await args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_wait"),
                    tool_name: ToolName::new(::tools::concurrency::AWAIT_TOOL_NAME),
                    arguments_ref: wait_args,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke");

        let ToolBatchOutcome::Deferred {
            call_id,
            completed_results,
            spec,
            ..
        } = result
        else {
            panic!("expected deferred await batch");
        };
        assert_eq!(call_id, ToolCallId::new("call_wait"));
        assert!(completed_results.is_empty());
        assert_eq!(spec.promise_ids, vec![PromiseId::new("promise_job")]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_emits_promise_effect_without_fleet_runtime() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent_no_fleet_cancel");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                display_name: None,
                created_at_ms: 1,
            })
            .await
            .expect("create parent");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: parent.clone(),
                expected_head: None,
                events: vec![
                    engine::CoreAgentCodec
                        .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                            observed_at_ms: 3,
                            joins: Default::default(),
                            event: engine::CoreAgentEvent::Promise(engine::PromiseEvent::Created {
                                promise: engine::Promise {
                                    promise_id: engine::PromiseId::new("promise_job"),
                                    source: engine::PromiseSource::Timer { fire_at_ms: 60_000 },
                                    scope: engine::PromiseScope::Session,
                                    status: engine::PromiseStatus::Pending,
                                    payload_ref: None,
                                    error_ref: None,
                                    deadline_ms: None,
                                },
                            }),
                        })
                        .expect("encode promise"),
                ],
            })
            .await
            .expect("append promise");
        let session_store: Arc<dyn SessionStore> = sessions;
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_session_store(session_store);
        let cancel_args = blobs
            .put_bytes(br#"{"promises":["promise_job"]}"#.to_vec())
            .await
            .expect("cancel args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_cancel"),
                    tool_name: ToolName::new(::tools::concurrency::CANCEL_TOOL_NAME),
                    arguments_ref: cancel_args,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        assert_eq!(result.results[0].effects.len(), 1);
        assert_eq!(
            result.results[0].effects[0].kind,
            engine::PROMISE_CANCEL_EFFECT_KIND
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detach_emits_promise_effect_without_fleet_runtime() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent_no_fleet_detach");
        sessions
            .create_session(CreateSession {
                session_id: parent.clone(),
                display_name: None,
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
        let mut events =
            engine::core_agent_clone_opening_events(&state, 2).expect("opening events");
        events.push(
            engine::CoreAgentCodec
                .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                    observed_at_ms: 3,
                    joins: Default::default(),
                    event: engine::CoreAgentEvent::Run(engine::RunEvent::Accepted(
                        engine::AcceptedRunEvent {
                            run_id: RunId::new(1),
                            submission_id: None,
                            origin: Default::default(),
                            source: engine::RunSource::Input { input: Vec::new() },
                            run_config: Default::default(),
                            config_revision: 0,
                            notify_on_terminal: Vec::new(),
                        },
                    )),
                })
                .expect("encode run"),
        );
        events.push(
            engine::CoreAgentCodec
                .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                    observed_at_ms: 4,
                    joins: Default::default(),
                    event: engine::CoreAgentEvent::Run(engine::RunEvent::Started {
                        run_id: RunId::new(1),
                    }),
                })
                .expect("encode run start"),
        );
        events.push(
            engine::CoreAgentCodec
                .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                    observed_at_ms: 5,
                    joins: Default::default(),
                    event: engine::CoreAgentEvent::Promise(engine::PromiseEvent::Created {
                        promise: engine::Promise {
                            promise_id: engine::PromiseId::new("promise_job"),
                            source: engine::PromiseSource::Timer { fire_at_ms: 60_000 },
                            scope: engine::PromiseScope::Run {
                                run_id: RunId::new(1),
                            },
                            status: engine::PromiseStatus::Pending,
                            payload_ref: None,
                            error_ref: None,
                            deadline_ms: None,
                        },
                    }),
                })
                .expect("encode promise"),
        );
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: parent.clone(),
                expected_head: None,
                events,
            })
            .await
            .expect("append state");
        let session_store: Arc<dyn SessionStore> = sessions;
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog)
            .with_session_store(session_store);
        let detach_args = blobs
            .put_bytes(br#"{"promises":["promise_job"]}"#.to_vec())
            .await
            .expect("detach args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: parent,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_detach"),
                    tool_name: ToolName::new(::tools::concurrency::DETACH_TOOL_NAME),
                    arguments_ref: detach_args,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed");

        if result.results[0].status != ToolCallStatus::Succeeded {
            let error = if let Some(error_ref) = result.results[0].error_ref.as_ref() {
                blobs.read_text(error_ref).await.expect("read error")
            } else {
                String::new()
            };
            panic!("detach failed: {error}");
        }
        assert_eq!(result.results[0].effects.len(), 1);
        assert_eq!(
            result.results[0].effects[0].kind,
            engine::PROMISE_DETACH_EFFECT_KIND
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sleep_emits_timer_promise_effect_without_fleet_runtime() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog.clone(), catalog);
        let sleep_args = blobs
            .put_bytes(br#"{"ms":50}"#.to_vec())
            .await
            .expect("sleep args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_sleep"),
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                default_targets: Default::default(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_sleep"),
                    tool_name: ToolName::new(::tools::concurrency::SLEEP_TOOL_NAME),
                    arguments_ref: sleep_args,
                    execution_target: None,
                }],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed");

        assert_eq!(result.results[0].status, ToolCallStatus::Succeeded);
        assert_eq!(result.results[0].effects.len(), 1);
        let effect = &result.results[0].effects[0];
        assert_eq!(effect.kind, engine::PROMISE_CREATE_EFFECT_KIND);
        assert_eq!(effect.data.get("source"), Some(&"timer".to_owned()));
        assert!(effect.data.contains_key("fire_at_ms"));
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
