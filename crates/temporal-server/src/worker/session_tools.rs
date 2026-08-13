use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_trait::async_trait;
use engine::{
    BlobRef, CoreAgentIoError, CoreAgentTools, PromiseId, PromiseSource, ProviderApiKind,
    SessionId, ToolBatchOutcome, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationResult, promise_create_effect,
    storage::{BlobEdge, BlobGraphStore, BlobStore, BlobStoreError, SessionStore},
};
use environments::{EnvironmentId, EnvironmentRecord, EnvironmentRegistryError, EnvironmentStore};
use host_client::{HostClientError, HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        jobs::{JobReadResult as HostJobReadResult, ReadJobsParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostConnectionSpec, HostTransport},
};
use store_pg::PgStore;
use tools::{
    concurrency::{
        AWAIT_TOOL_NAME, AwaitArgs, CANCEL_TOOL_NAME, CancelArgs, DETACH_TOOL_NAME, DetachArgs,
        SLEEP_TOOL_NAME, SleepArgs, SleepOutput, cancel_promises_from_runtime,
        cancel_promises_model_visible_text, detach_promises_from_runtime,
        detach_promises_model_visible_text, is_concurrency_tool, sleep_model_visible_text,
    },
    environment::control::{
        DEFAULT_ENVIRONMENT_LIST_LIMIT, ENVIRONMENT_ACTIVATE_TOOL_NAME,
        ENVIRONMENT_DEACTIVATE_TOOL_NAME, ENVIRONMENT_LIST_TOOL_NAME, ENVIRONMENT_READ_TOOL_NAME,
        EnvironmentActivateArgs, EnvironmentDeactivateArgs, EnvironmentListArgs,
        EnvironmentReadArgs, MAX_ENVIRONMENT_LIST_LIMIT, is_environment_control_tool,
        is_environment_selection_tool,
    },
    environment::jobs::{
        JOB_READ_TOOL_NAME, JOB_RUN_WORKFLOW_SEMANTIC_TYPE, JOB_RUN_WORKFLOW_TOOL_ID,
        JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE, JOB_SUBMIT_WORKFLOW_TOOL_ID, JobHandle, JobHandleArg,
        JobReadArgs, JobSubmitExecutionContextV1, ModelJobResult, ModelJobResultSet,
        is_environment_job_query_tool_name, normalize_job_result,
    },
    fleet::is_fleet_tool,
    fs::{FsPath, FsToolContext, LinkedVfsFileSystem},
    host_protocol::RemoteHostConnection,
    limits::ToolLimits,
    runtime::InlineToolRuntime,
    runtime::{ToolCatalog, ToolTarget},
    toolset::{EnvironmentToolsetConfig, ToolsetConfig, ToolsetEnvironment, resolve_toolset},
    web::fetch::WebFetchToolConfig,
    workflow_tool::invoke_workflow_tool,
};
use vfs::{ResolvedWorkspaceLink, VfsCatalogError, VfsWorkspaceStore};

use crate::{
    credential_injection::EnvironmentCredentialResolver,
    environment::{RuntimeEnvironment, SessionEnvironmentManager},
    fleet::{FleetChildRuntime, FleetService, FleetToolExecutor, await_spec_from_args},
};

#[derive(Clone)]
pub struct SessionTools {
    blobs: Arc<dyn BlobStore>,
    blob_graph: Option<Arc<dyn BlobGraphStore>>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    environments: SessionEnvironmentManager,
    environment_store: Option<Arc<dyn EnvironmentStore>>,
    environment_resolver: Option<crate::environment_resolver::EnvironmentResolver>,
    environment_credentials: Option<EnvironmentCredentialResolver>,
    environment_gateway: Option<crate::environment_gateway::EnvironmentGatewayClientConfig>,
    fleet: Option<FleetToolExecutor>,
}

impl SessionTools {
    pub fn new(blobs: Arc<dyn BlobStore>, workspace_store: Arc<dyn VfsWorkspaceStore>) -> Self {
        let environments = SessionEnvironmentManager::new(blobs.clone());
        Self {
            blobs,
            blob_graph: None,
            workspace_store,
            environments,
            environment_store: None,
            environment_resolver: None,
            environment_credentials: None,
            environment_gateway: None,
            fleet: None,
        }
    }

    pub fn with_fleet_runtime(
        mut self,
        sessions: Arc<dyn SessionStore>,
        runtime: Arc<dyn FleetChildRuntime>,
    ) -> Self {
        let service =
            FleetService::new(sessions, runtime).with_vfs_stores(self.workspace_store.clone());
        self.fleet = Some(FleetToolExecutor::new(self.blobs.clone(), service));
        self
    }

    pub fn with_environment_store(mut self, environments: Arc<dyn EnvironmentStore>) -> Self {
        self.environment_store = Some(environments);
        self
    }

    pub(crate) fn with_environment_resolver(
        mut self,
        resolver: crate::environment_resolver::EnvironmentResolver,
    ) -> Self {
        self.environment_resolver = Some(resolver);
        self
    }

    pub(crate) fn with_environment_credentials(
        mut self,
        credentials: EnvironmentCredentialResolver,
    ) -> Self {
        self.environment_credentials = Some(credentials);
        self
    }

    pub(crate) fn with_environment_gateway(
        mut self,
        gateway: crate::environment_gateway::EnvironmentGatewayClientConfig,
    ) -> Self {
        if let Some(resolver) = self.environment_resolver.take() {
            self.environment_resolver = Some(resolver.with_gateway(gateway.clone()));
        }
        self.environment_gateway = Some(gateway);
        self
    }

    pub fn with_environment(mut self, environment: RuntimeEnvironment) -> Self {
        self.environments.insert_environment(environment);
        self
    }

    pub fn from_pg_store(store: Arc<PgStore>) -> Self {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let blob_graph: Arc<dyn BlobGraphStore> = store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = store.clone();
        let environments: Arc<dyn EnvironmentStore> = store.clone();
        let credentials = EnvironmentCredentialResolver::from_pg_store(store.clone());
        let resolver =
            crate::environment_resolver::EnvironmentResolver::from_pg_store(store.clone());
        Self::new(blobs, workspace_store)
            .with_blob_graph(blob_graph)
            .with_environment_store(environments)
            .with_environment_resolver(resolver)
            .with_environment_credentials(credentials)
    }

    fn with_blob_graph(mut self, blob_graph: Arc<dyn BlobGraphStore>) -> Self {
        self.blob_graph = Some(blob_graph);
        self
    }

    pub fn from_pg_store_with_fleet_runtime(
        store: Arc<PgStore>,
        runtime: Arc<dyn FleetChildRuntime>,
    ) -> Self {
        let sessions: Arc<dyn engine::storage::SessionStore> = store.clone();
        Self::from_pg_store(store).with_fleet_runtime(sessions, runtime)
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
                    fleet_policy: request.fleet_policy.clone(),
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
            CANCEL_TOOL_NAME => self.invoke_cancel_call(call).await,
            DETACH_TOOL_NAME => self.invoke_detach_call(request.run_id, call).await,
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

    async fn invoke_cancel_call(
        &self,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let result = match async {
            let args: CancelArgs = self.read_tool_args(call).await?;
            cancel_promises_from_runtime(&args, call.promise_control.as_ref()).map_err(io_error)
        }
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

    async fn invoke_detach_call(
        &self,
        run_id: engine::RunId,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let result = match async {
            let args: DetachArgs = self.read_tool_args(call).await?;
            detach_promises_from_runtime(&args, run_id, call.promise_control.as_ref())
                .map_err(io_error)
        }
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
                        fleet_policy: request.fleet_policy.clone(),
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
            fleet_policy: request.fleet_policy.clone(),
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
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_name.as_str() {
            JOB_READ_TOOL_NAME => {
                let args: JobReadArgs = self.read_tool_args(call).await?;
                let result = self
                    .read_environment_jobs(
                        &request.session_id,
                        environments,
                        args.jobs,
                        args.output_bytes,
                        args.after_seq,
                        args.include_artifacts,
                    )
                    .await?;
                self.succeeded_job_read_result(call, result.entries).await
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

    async fn read_environment_jobs(
        &self,
        session_id: &SessionId,
        environments: &SessionEnvironmentManager,
        handles: Vec<JobHandleArg>,
        output_bytes: Option<usize>,
        after_seq: Option<u64>,
        include_artifacts: bool,
    ) -> Result<EnvironmentJobRead, CoreAgentIoError> {
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match self.resolve_job_handle_arg(session_id, handle) {
                Ok(handle) => handle,
                Err(error) => {
                    entries.push(model_job_error(None, error));
                    continue;
                }
            };
            let environment_id = match EnvironmentId::try_new(resolved.environment_id.clone()) {
                Ok(environment_id) => environment_id,
                Err(error) => {
                    entries.push(model_job_error(
                        Some(resolved),
                        format!("invalid job handle environment_id: {error}"),
                    ));
                    continue;
                }
            };
            let environment = if let Some(environment) =
                environments.environment(environment_id.as_str()).cloned()
            {
                Ok(environment)
            } else if let Some(store) = self.environment_store.as_ref() {
                match store.read_environment(&environment_id).await {
                    Ok(resource) => {
                        self.runtime_environment_for_resource(session_id, resource)
                            .await
                    }
                    Err(error) => Err(map_environments_error(error)),
                }
            } else {
                Err(io_error(
                    "environment store is not configured on this runtime",
                ))
            };
            let environment = match environment {
                Ok(environment) => environment,
                Err(error) => {
                    entries.push(model_job_error(
                        Some(resolved),
                        format!("environment instance is not reachable: {error}"),
                    ));
                    continue;
                }
            };
            let Some(jobs) = environment.tool_context().jobs.as_ref() else {
                entries.push(model_job_error(
                    Some(resolved),
                    format!("environment does not support durable jobs: {environment_id}"),
                ));
                continue;
            };
            match jobs
                .read_jobs(ReadJobsParams {
                    namespace: environment_id.as_str().to_owned(),
                    jobs: vec![resolved.job_id.clone()],
                    after_seq,
                    max_bytes: output_bytes,
                    include_artifacts,
                    wait_ms: None,
                })
                .await
            {
                Ok(response) => {
                    entries.push(
                        job_read_entry_from_response(
                            self.blobs.as_ref(),
                            resolved,
                            response.jobs.into_iter().next(),
                            output_bytes,
                        )
                        .await?,
                    );
                }
                Err(error) => {
                    entries.push(model_job_error(Some(resolved), error.to_string()));
                }
            }
        }
        Ok(EnvironmentJobRead { entries })
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

    async fn invoke_workflow_tool_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        binding: &engine::WorkflowToolBinding,
        emitted_count: u32,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        if emitted_count >= engine::MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                format!(
                    "workflow tool {} reached its per-run emission cap of {}",
                    binding.definition.tool_id,
                    engine::MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN
                ),
            )
            .await;
        }
        let is_environment_job_workflow_tool = matches!(
            (
                binding.definition.tool_id.as_str(),
                binding.definition.semantic_type.as_str()
            ),
            (
                JOB_SUBMIT_WORKFLOW_TOOL_ID,
                JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE
            ) | (JOB_RUN_WORKFLOW_TOOL_ID, JOB_RUN_WORKFLOW_SEMANTIC_TYPE)
        );
        let execution_context_ref = if is_environment_job_workflow_tool {
            let Some(environment_id) = request.active_environment_id.as_ref() else {
                return failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!(
                        "{} requires an active environment",
                        binding.definition.tool.name
                    ),
                )
                .await;
            };
            let allowed_provider_ids = supplied_environment_policy(request)?
                .map(|providers| providers.into_iter().collect());
            let context = JobSubmitExecutionContextV1::new(
                environment_id.as_str().to_owned(),
                allowed_provider_ids,
            );
            Some(
                self.blobs
                    .put_bytes(serde_json::to_vec(&context).map_err(io_error)?)
                    .await
                    .map_err(map_blob_error)?,
            )
        } else {
            None
        };
        match invoke_workflow_tool(
            self.blobs.as_ref(),
            binding,
            &request.session_id,
            request.run_id,
            request.turn_id,
            request.batch_id,
            call,
            execution_context_ref,
            now_unix_ms()?,
        )
        .await
        {
            Ok(output) => {
                let mut result = self
                    .succeeded_tool_result(call, &output.output_json, output.model_visible_text)
                    .await?;
                result.effects = output.effects;
                Ok(result)
            }
            Err(error) => {
                failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string()).await
            }
        }
    }

    async fn invoke_supplied_workflow_tool_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        successful_siblings: &mut BTreeMap<engine::WorkflowToolId, u32>,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let runtime = call
            .workflow_tool
            .as_ref()
            .expect("supplied workflow-tool dispatch requires runtime facts");
        if runtime.version != engine::WorkflowToolCallRuntime::VERSION {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                format!(
                    "unsupported workflow-tool runtime facts version {}",
                    runtime.version
                ),
            )
            .await;
        }
        let tool_id = runtime.binding.definition.tool_id.clone();
        let sibling_count = successful_siblings.get(&tool_id).copied().unwrap_or(0);
        let emitted_count = runtime.prior_emission_count.saturating_add(sibling_count);
        let result = self
            .invoke_workflow_tool_call(request, call, &runtime.binding, emitted_count)
            .await?;
        if result.status == ToolCallStatus::Succeeded {
            successful_siblings.insert(tool_id, sibling_count.saturating_add(1));
        }
        Ok(result)
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

    async fn succeeded_job_read_result(
        &self,
        call: &engine::ToolInvocationRequest,
        jobs: Vec<ModelJobResult>,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let output = ModelJobResultSet { jobs };
        let output_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&output).map_err(io_error)?)
            .await
            .map_err(map_blob_error)?;
        let edges = output
            .jobs
            .iter()
            .flat_map(|job| &job.output)
            .filter_map(|segment| {
                segment
                    .blob_ref
                    .clone()
                    .map(|child| BlobEdge::contains(output_ref.clone(), child))
            })
            .collect::<Vec<_>>();
        if !edges.is_empty()
            && let Some(blob_graph) = &self.blob_graph
        {
            blob_graph
                .record_blob_edges(edges)
                .await
                .map_err(map_blob_error)?;
        }
        Ok(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref.clone()),
            model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
                &call.call_id,
                ToolCallStatus::Succeeded,
                output_ref,
            )],
            error_ref: None,
            effects: Vec::new(),
        })
    }

    async fn invoke_environment_control_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let Some(resolver) = self.environment_resolver.as_ref() else {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id.clone(),
                "environment resolver is not configured on this runtime",
            )
            .await;
        };
        let allowed = supplied_environment_policy(request)?;
        let active = request.active_environment_id.as_ref();
        match call.tool_name.as_str() {
            ENVIRONMENT_LIST_TOOL_NAME => {
                let args: EnvironmentListArgs = self.read_tool_args(call).await?;
                let limit = args
                    .limit
                    .unwrap_or(DEFAULT_ENVIRONMENT_LIST_LIMIT)
                    .clamp(1, MAX_ENVIRONMENT_LIST_LIMIT);
                let mut environments = match resolver.list_allowed(allowed.as_ref()).await {
                    Ok(environments) => environments,
                    Err(error) => {
                        return failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            error.to_string(),
                        )
                        .await;
                    }
                };
                if let Some(cursor) = args.cursor.as_deref() {
                    environments.retain(|environment| environment.environment_id.as_str() > cursor);
                }
                let has_more = environments.len() > limit;
                environments.truncate(limit);
                let next_cursor = has_more
                    .then(|| environments.last())
                    .flatten()
                    .map(|environment| environment.environment_id.as_str().to_owned());
                let output = serde_json::json!({
                    "environments": environments.iter().map(|environment| {
                        environment_model_view(environment, active)
                    }).collect::<Vec<_>>(),
                    "next_cursor": next_cursor,
                });
                self.succeeded_tool_result(
                    call,
                    &output,
                    serde_json::to_string_pretty(&output).map_err(io_error)?,
                )
                .await
            }
            ENVIRONMENT_READ_TOOL_NAME => {
                let args: EnvironmentReadArgs = self.read_tool_args(call).await?;
                let environment_id = match environment_read_target(args, active) {
                    Ok(environment_id) => environment_id,
                    Err(EnvironmentReadTargetError::NoActiveEnvironment) => {
                        return failed_structured_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            "no_active_environment",
                            "No active environment is selected for this session.",
                        )
                        .await;
                    }
                    Err(EnvironmentReadTargetError::InvalidEnvironmentId(message)) => {
                        return failed_result(self.blobs.as_ref(), call.call_id.clone(), message)
                            .await;
                    }
                };
                let environment = match resolver
                    .read_allowed(&environment_id, allowed.as_ref())
                    .await
                {
                    Ok(environment) => environment,
                    Err(error) => {
                        return failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            error.to_string(),
                        )
                        .await;
                    }
                };
                let output = environment_model_view(&environment, active);
                self.succeeded_tool_result(
                    call,
                    &output,
                    serde_json::to_string_pretty(&output).map_err(io_error)?,
                )
                .await
            }
            ENVIRONMENT_ACTIVATE_TOOL_NAME => {
                let args: EnvironmentActivateArgs = self.read_tool_args(call).await?;
                let environment_id = match EnvironmentId::try_new(args.environment_id) {
                    Ok(id) => id,
                    Err(error) => {
                        return failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            error.to_string(),
                        )
                        .await;
                    }
                };
                let environment = match resolver
                    .selectable(
                        &environment_id,
                        allowed.as_ref(),
                        i64::try_from(now_unix_ms()?).map_err(io_error)?,
                    )
                    .await
                {
                    Ok(environment) => environment,
                    Err(error) => {
                        return failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            error.to_string(),
                        )
                        .await;
                    }
                };
                let output = serde_json::json!({
                    "environment_id": environment.environment_id.as_str(),
                    "active": true,
                });
                let mut result = self
                    .succeeded_tool_result(
                        call,
                        &output,
                        format!("Active environment set to {}.", environment.environment_id),
                    )
                    .await?;
                result.effects.push(engine::environment_activate_effect(
                    &environment.environment_id,
                ));
                Ok(result)
            }
            ENVIRONMENT_DEACTIVATE_TOOL_NAME => {
                let _: EnvironmentDeactivateArgs = self.read_tool_args(call).await?;
                let output = serde_json::json!({ "active": false });
                let mut result = self
                    .succeeded_tool_result(call, &output, "Active environment cleared.")
                    .await?;
                result.effects.push(engine::environment_deactivate_effect());
                Ok(result)
            }
            other => {
                failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("unknown environment control tool {other}"),
                )
                .await
            }
        }
    }

    fn resolve_job_handle_arg(
        &self,
        _current_session_id: &SessionId,
        handle: JobHandleArg,
    ) -> Result<JobHandle, String> {
        let environment_id = EnvironmentId::try_new(handle.environment_id)
            .map_err(|error| format!("invalid job handle environment_id: {error}"))?;
        Ok(JobHandle {
            environment_id: environment_id.as_str().to_owned(),
            job_id: handle.job_id,
        })
    }

    async fn environment_manager_for_session(
        &self,
        request: &ToolInvocationBatchRequest,
    ) -> Result<SessionEnvironmentManager, CoreAgentIoError> {
        let mut environments = self.environments.clone();
        let Some(environment_id) = request.active_environment_id.as_ref() else {
            return Ok(environments);
        };
        let allowed = supplied_environment_policy(request)?;
        if environments.environment(environment_id.as_str()).is_some() {
            return Ok(environments);
        }
        let resource = if let Some(resolver) = self.environment_resolver.as_ref() {
            match resolver
                .selectable(
                    environment_id,
                    allowed.as_ref(),
                    i64::try_from(now_unix_ms()?)
                        .map_err(|_| io_error("current timestamp does not fit in i64"))?,
                )
                .await
            {
                Ok(resource) => resource,
                Err(crate::environment_resolver::EnvironmentResolveError::Store(
                    environments::EnvironmentRegistryError::Store { message },
                )) => return Err(io_error(message)),
                Err(_) => return Ok(environments),
            }
        } else {
            let store = self
                .environment_store
                .as_ref()
                .ok_or_else(|| io_error("environment store is not configured on this runtime"))?;
            let resource = match store.read_environment(environment_id).await {
                Ok(resource) => resource,
                Err(environments::EnvironmentRegistryError::Store { message }) => {
                    return Err(io_error(message));
                }
                Err(_) => return Ok(environments),
            };
            if allowed.as_ref().is_some_and(|providers| {
                resource
                    .provider_id()
                    .is_none_or(|id| !providers.contains(id.as_str()))
            }) {
                return Ok(environments);
            }
            resource
        };
        environments.insert_environment(
            self.runtime_environment_for_resource(&request.session_id, resource)
                .await?,
        );
        Ok(environments)
    }

    async fn runtime_environment_for_resource(
        &self,
        session_id: &SessionId,
        resource: EnvironmentRecord,
    ) -> Result<RuntimeEnvironment, CoreAgentIoError> {
        let gateway = self
            .environment_gateway
            .as_ref()
            .ok_or_else(|| io_error("environment gateway is not configured on this worker"))?;
        let connection = gateway.connection_for(
            self.environment_resolver
                .as_ref()
                .map(|resolver| resolver.universe_id())
                .unwrap_or_default(),
            &resource,
        );
        let mut client = connect_host_data_client(
            &connection,
            gateway.connect_options("lightspeed-temporal-server"),
        )
        .await?;
        let response = client
            .initialize(&InitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "lightspeed-temporal-server".to_owned(),
                scope: connection.scope.clone(),
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
            environment_context =
                credentials.wrap_context(environment_context, resource.environment_id.clone());
        }
        let environment_context =
            environment_context.with_session_id(session_id.as_str().to_owned());
        Ok(RuntimeEnvironment::from_resource(
            resource,
            environment_context,
            fs_context,
        ))
    }

    fn runtime_for_domains(
        &self,
        links: Vec<ResolvedWorkspaceLink>,
        environments: &SessionEnvironmentManager,
        active_environment_id: Option<&EnvironmentId>,
    ) -> Result<InlineToolRuntime, CoreAgentIoError> {
        let catalog = runtime_catalog(true, true)?;
        let vfs = if links.is_empty() {
            None
        } else {
            let fs = LinkedVfsFileSystem::new(
                self.blobs.clone(),
                self.workspace_store.clone(),
                links.clone(),
            )
            .map_err(io_error)?;
            let cwd = linked_vfs_cwd(fs.links())?;
            Some(FsToolContext::new(Arc::new(fs), self.blobs.clone()).with_cwd(cwd))
        };
        let environment =
            active_environment_id.and_then(|id| environments.active_tool_context(id.as_str()));
        Ok(InlineToolRuntime::with_contexts_and_blob_store(
            vfs,
            environment,
            self.blobs.clone(),
            ToolLimits::default(),
            catalog,
        ))
    }
}

struct EnvironmentJobRead {
    entries: Vec<ModelJobResult>,
}

fn environment_model_view(
    environment: &EnvironmentRecord,
    active: Option<&EnvironmentId>,
) -> serde_json::Value {
    serde_json::json!({
        "environment_id": environment.environment_id.as_str(),
        "provider_id": environment.provider_id().map(|id| id.as_str()),
        "display_name": environment.display_name,
        "status": format!("{:?}", environment.status).to_lowercase(),
        "active": active == Some(&environment.environment_id),
        "observed_at_ms": environment.observed_at_ms(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EnvironmentReadTargetError {
    NoActiveEnvironment,
    InvalidEnvironmentId(String),
}

fn environment_read_target(
    args: EnvironmentReadArgs,
    active: Option<&EnvironmentId>,
) -> Result<EnvironmentId, EnvironmentReadTargetError> {
    match args.environment_id {
        Some(environment_id) => EnvironmentId::try_new(environment_id)
            .map_err(|error| EnvironmentReadTargetError::InvalidEnvironmentId(error.to_string())),
        None => active
            .cloned()
            .ok_or(EnvironmentReadTargetError::NoActiveEnvironment),
    }
}

fn supplied_environment_policy(
    request: &ToolInvocationBatchRequest,
) -> Result<Option<BTreeSet<String>>, CoreAgentIoError> {
    let policy = request
        .environment_policy
        .as_ref()
        .ok_or_else(|| io_error("environment runtime policy is missing"))?;
    if policy.version != engine::EnvironmentPolicyRuntime::VERSION {
        return Err(io_error(format!(
            "unsupported environment runtime policy version {}",
            policy.version
        )));
    }
    Ok(policy
        .allowed_provider_ids
        .as_ref()
        .map(|providers| providers.iter().cloned().collect()))
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

async fn job_read_entry_from_response(
    blobs: &dyn BlobStore,
    handle: JobHandle,
    response: Option<HostJobReadResult>,
    output_bytes: Option<usize>,
) -> Result<ModelJobResult, CoreAgentIoError> {
    match response {
        Some(response) => normalize_job_result(
            blobs,
            Some(handle),
            Some(response.summary),
            response.output_chunks,
            response.output_next_seq,
            response.artifacts,
            None,
            output_bytes,
        )
        .await
        .map_err(map_blob_error),
        None => Ok(model_job_error(
            Some(handle),
            "provider returned no job result".to_owned(),
        )),
    }
}

fn model_job_error(handle: Option<JobHandle>, error: String) -> ModelJobResult {
    ModelJobResult {
        handle,
        summary: None,
        output: Vec::new(),
        output_next_seq: 0,
        truncated: false,
        artifacts: Vec::new(),
        error: Some(error),
    }
}

async fn connect_host_data_client(
    connection: &HostConnectionSpec,
    options: WebSocketConnectOptions,
) -> Result<HostDataClient<host_client::WebSocketTransport>, CoreAgentIoError> {
    match &connection.transport {
        HostTransport::WebSocket => HostDataClient::connect(&connection.endpoint, options)
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

fn runtime_catalog(
    include_environment_tools: bool,
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
        if include_environment_tools {
            config.builtin.environment = EnvironmentToolsetConfig::basic();
        }
        if include_job_tools {
            config.builtin.environment.job_read = true;
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
        let routing_catalog = runtime_catalog(true, true)?;
        let selection_calls = request
            .calls
            .iter()
            .filter(|call| is_environment_selection_tool(&call.tool_name))
            .count();
        let mixes_environment_dependency = selection_calls > 0
            && request.calls.iter().any(|call| {
                !is_environment_selection_tool(&call.tool_name)
                    && routing_catalog.get(&call.tool_name).is_some_and(|binding| {
                        binding.logical_id.starts_with("env.")
                            && binding.logical_id != "env.job_read"
                    })
            });
        if selection_calls > 1 || mixes_environment_dependency {
            let mut results = Vec::with_capacity(request.calls.len());
            for call in &request.calls {
                results.push(
                    failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        "environment activation/deactivation cannot share a batch with another selection or an environment-dependent tool",
                    )
                    .await?,
                );
            }
            return Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results,
            }));
        }
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
            !is_fleet_tool(&call.tool_name)
                && !is_concurrency_tool(&call.tool_name)
                && !is_environment_control_tool(&call.tool_name)
                && call.workflow_tool.is_none()
        });
        let mut successful_workflow_siblings = BTreeMap::new();
        if !has_generic_runtime_call {
            // Fleet/concurrency-only batches skip generic VFS/runtime setup entirely.
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
                } else if call.workflow_tool.is_some() {
                    results.push(
                        self.invoke_supplied_workflow_tool_call(
                            &request,
                            call,
                            &mut successful_workflow_siblings,
                        )
                        .await?,
                    );
                } else if is_fleet_tool(&call.tool_name) {
                    results.push(self.invoke_fleet_call(&request, call).await?);
                } else if is_environment_control_tool(&call.tool_name) {
                    results.push(self.invoke_environment_control_call(&request, call).await?);
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

        let has_vfs_call = request.calls.iter().any(|call| {
            routing_catalog
                .get(&call.tool_name)
                .is_some_and(|binding| binding.logical_id.starts_with("vfs."))
        });
        let has_environment_call = request.calls.iter().any(|call| {
            routing_catalog
                .get(&call.tool_name)
                .is_some_and(|binding| binding.logical_id.starts_with("env."))
                || is_environment_job_query_tool_name(call.tool_name.as_str())
        });
        let links = if has_vfs_call {
            vfs::resolve_workspace_links(
                self.blobs.clone(),
                self.workspace_store.clone(),
                &request.workspace_links,
            )
            .await
            .map_err(map_catalog_error)?
        } else {
            Vec::new()
        };
        let environments = if has_environment_call {
            self.environment_manager_for_session(&request).await?
        } else {
            SessionEnvironmentManager::new(self.blobs.clone())
        };
        let runtime =
            self.runtime_for_domains(links, &environments, request.active_environment_id.as_ref())?;

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
            } else if call.workflow_tool.is_some() {
                results.push(
                    self.invoke_supplied_workflow_tool_call(
                        &request,
                        call,
                        &mut successful_workflow_siblings,
                    )
                    .await?,
                );
            } else if is_fleet_tool(&call.tool_name) {
                results.push(self.invoke_fleet_call(&request, call).await?);
            } else if is_concurrency_tool(&call.tool_name) {
                results.push(self.invoke_concurrency_call(&request, call).await?);
            } else if is_environment_control_tool(&call.tool_name) {
                results.push(self.invoke_environment_control_call(&request, call).await?);
            } else if is_environment_job_query_tool_name(call.tool_name.as_str()) {
                results.push(
                    self.invoke_environment_job_call(&request, call, &environments)
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

    async fn invoke_call(
        &self,
        request: engine::ToolInvocationCallRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let call = request.call.clone();
        // Batch-unit tools never arrive here: the workflow routes batches
        // containing them through the batch activity.
        if call.workflow_tool.is_some() || call.tool_name.as_str() == AWAIT_TOOL_NAME {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id,
                "this tool call requires batch-unit execution",
            )
            .await;
        }
        let routing_catalog = runtime_catalog(true, true)?;
        if let Some(message) = per_call_batch_rule_violation(&routing_catalog, &request) {
            return failed_result(self.blobs.as_ref(), call.call_id, message).await;
        }
        let batch_request = request.into_batch_request();
        if is_fleet_tool(&call.tool_name) {
            return self.invoke_fleet_call(&batch_request, &call).await;
        }
        if is_concurrency_tool(&call.tool_name) {
            return self.invoke_concurrency_call(&batch_request, &call).await;
        }
        if is_environment_control_tool(&call.tool_name) {
            return self
                .invoke_environment_control_call(&batch_request, &call)
                .await;
        }
        if is_environment_job_query_tool_name(call.tool_name.as_str()) {
            let environments = self.environment_manager_for_session(&batch_request).await?;
            return self
                .invoke_environment_job_call(&batch_request, &call, &environments)
                .await;
        }
        let is_vfs_call = routing_catalog
            .get(&call.tool_name)
            .is_some_and(|binding| binding.logical_id.starts_with("vfs."));
        let is_environment_call = routing_catalog
            .get(&call.tool_name)
            .is_some_and(|binding| binding.logical_id.starts_with("env."));
        let links = if is_vfs_call {
            vfs::resolve_workspace_links(
                self.blobs.clone(),
                self.workspace_store.clone(),
                &batch_request.workspace_links,
            )
            .await
            .map_err(map_catalog_error)?
        } else {
            Vec::new()
        };
        let environments = if is_environment_call {
            self.environment_manager_for_session(&batch_request).await?
        } else {
            SessionEnvironmentManager::new(self.blobs.clone())
        };
        let runtime = self.runtime_for_domains(
            links,
            &environments,
            batch_request.active_environment_id.as_ref(),
        )?;
        runtime.invoke_call(&call).await
    }
}

/// Cross-call batch rules evaluated from bounded sibling summaries. Only the
/// calls participating in a violation fail; unrelated siblings execute
/// normally.
fn per_call_batch_rule_violation(
    routing_catalog: &ToolCatalog,
    request: &engine::ToolInvocationCallRequest,
) -> Option<&'static str> {
    let call = &request.call;
    let is_env_dependent = |tool_name: &engine::ToolName| {
        routing_catalog.get(tool_name).is_some_and(|binding| {
            binding.logical_id.starts_with("env.") && binding.logical_id != "env.job_read"
        })
    };
    let sibling_selection = request
        .sibling_calls
        .iter()
        .any(|sibling| is_environment_selection_tool(&sibling.tool_name));
    if is_environment_selection_tool(&call.tool_name)
        && (sibling_selection
            || request
                .sibling_calls
                .iter()
                .any(|sibling| is_env_dependent(&sibling.tool_name)))
    {
        return Some(
            "environment activation/deactivation cannot share a batch with another selection or an environment-dependent tool",
        );
    }
    if is_env_dependent(&call.tool_name) && sibling_selection {
        return Some(
            "environment activation/deactivation cannot share a batch with another selection or an environment-dependent tool",
        );
    }
    let is_fleet_message = |tool_name: &engine::ToolName| {
        matches!(
            tool_name.as_str(),
            tools::fleet::AGENT_SEND_TOOL_NAME | tools::fleet::AGENT_REQUEST_TOOL_NAME
        )
    };
    // Argument refs are content-addressed, so ref equality is content
    // equality; this matches the batch path's byte comparison.
    if is_fleet_message(&call.tool_name)
        && request.sibling_calls.iter().any(|sibling| {
            is_fleet_message(&sibling.tool_name) && sibling.arguments_ref == call.arguments_ref
        })
    {
        return Some(
            "duplicate agent_send/agent_request calls with identical arguments in one tool batch are rejected",
        );
    }
    None
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

fn linked_vfs_cwd(links: &[ResolvedWorkspaceLink]) -> Result<FsPath, CoreAgentIoError> {
    let cwd = if links.iter().any(|link| link.path.as_str() == "/workspace") {
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
    failed_result_bytes(blobs, call_id, message.into().into_bytes()).await
}

async fn failed_structured_result(
    blobs: &dyn BlobStore,
    call_id: engine::ToolCallId,
    code: &str,
    message: &str,
) -> Result<ToolInvocationResult, CoreAgentIoError> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "code": code,
        "message": message,
    }))
    .map_err(io_error)?;
    failed_result_bytes(blobs, call_id, bytes).await
}

async fn failed_result_bytes(
    blobs: &dyn BlobStore,
    call_id: engine::ToolCallId,
    bytes: Vec<u8>,
) -> Result<ToolInvocationResult, CoreAgentIoError> {
    let error_ref = blobs.put_bytes(bytes).await.map_err(map_blob_error)?;
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
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use crate::environment::RuntimeEnvironment;
    use engine::{
        BlobRef, ContextEntryKind, FunctionToolSpec, RunId, SessionId, ToolBatchId, ToolCallId,
        ToolKind, ToolName, ToolParallelism, ToolSpec, TurnId, WorkflowEndpointRef,
        WorkflowToolDefinition, WorkflowToolId, WorkspaceLink, WorkspaceLinkAccess,
        WorkspaceLinkTarget,
        storage::{
            AppendSessionEvents, CreateSession, InMemoryBlobStore, InMemorySessionStore,
            SessionStore,
        },
    };
    use environments::{
        CreateEnvironment, EnvironmentIncarnationId, EnvironmentIncarnationRecord,
        EnvironmentProviderBindingId, EnvironmentProviderBindingStatus,
        EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProviderStore,
        EnvironmentProvisionRequestId, EnvironmentSource, EnvironmentStatus, EnvironmentStore,
        EnvironmentTemplateId, HostControllerConnectionSpec, InMemoryEnvironmentRegistryStore,
        ObserveProvisionedEnvironment, PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };
    use host_protocol::shared::{HostTargetId, HostTransport};
    use tools::environment::{
        EnvironmentToolContext,
        process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput, ProcessRequest,
            ProcessStatus, StreamOutput, WriteProcessStdinRequest,
        },
    };
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateInlineSnapshotRequest, CreateVfsWorkspaceRecord,
        InlineFile, VfsWorkspaceId, VfsWorkspaceRecord, create_inline_snapshot,
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

    fn per_call_request(
        tool_name: &str,
        arguments: &[u8],
        siblings: &[(&str, &[u8])],
    ) -> engine::ToolInvocationCallRequest {
        engine::ToolInvocationCallRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            fleet_policy: None,
            call: engine::ToolInvocationRequest {
                call_id: ToolCallId::new("call_self"),
                tool_name: ToolName::new(tool_name),
                arguments_ref: BlobRef::from_bytes(arguments),
                workflow_tool: None,
                promise_control: None,
            },
            sibling_calls: siblings
                .iter()
                .enumerate()
                .map(|(index, (name, arguments))| engine::ToolCallSummary {
                    call_id: ToolCallId::new(format!("call_sibling_{index}")),
                    tool_name: ToolName::new(*name),
                    arguments_ref: BlobRef::from_bytes(arguments),
                })
                .collect(),
            execution: engine::ToolExecutionSpec::default(),
        }
    }

    #[test]
    fn per_call_batch_rules_flag_only_participating_calls() {
        let catalog = runtime_catalog(true, true).expect("routing catalog");

        // A selection call with an environment-dependent sibling fails, and
        // an environment-dependent call with a selection sibling fails.
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request("environment_activate", b"{}", &[("read_file", b"{}")]),
            )
            .is_some()
        );
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request("read_file", b"{}", &[("environment_activate", b"{}")]),
            )
            .is_some()
        );
        // Two selection calls in one batch both fail.
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request(
                    "environment_activate",
                    b"{}",
                    &[("environment_deactivate", b"{}")],
                ),
            )
            .is_some()
        );
        // An unrelated sibling in the same violating batch is untouched.
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request(
                    "web_fetch",
                    b"{}",
                    &[("environment_activate", b"{}"), ("read_file", b"{}")],
                ),
            )
            .is_none()
        );
        // A lone selection call is allowed.
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request("environment_activate", b"{}", &[("web_fetch", b"{}")]),
            )
            .is_none()
        );
        // Duplicate fleet messages are content-addressed duplicates.
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request(
                    "agent_send",
                    b"{\"to\":\"a\"}",
                    &[("agent_request", b"{\"to\":\"a\"}")]
                ),
            )
            .is_some()
        );
        assert!(
            per_call_batch_rule_violation(
                &catalog,
                &per_call_request(
                    "agent_send",
                    b"{\"to\":\"a\"}",
                    &[("agent_send", b"{\"to\":\"b\"}")]
                ),
            )
            .is_none()
        );
    }

    #[test]
    fn environment_read_defaults_to_active_and_accepts_an_explicit_id() {
        let active = EnvironmentId::new("environment_active");
        assert_eq!(
            environment_read_target(EnvironmentReadArgs::default(), Some(&active)),
            Ok(active.clone())
        );
        assert_eq!(
            environment_read_target(
                EnvironmentReadArgs {
                    environment_id: Some("environment_other".to_owned()),
                },
                Some(&active),
            ),
            Ok(EnvironmentId::new("environment_other"))
        );
        assert_eq!(
            environment_read_target(EnvironmentReadArgs::default(), None),
            Err(EnvironmentReadTargetError::NoActiveEnvironment)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_active_environment_failure_is_structured_and_model_visible() {
        let blobs = InMemoryBlobStore::new();
        let result = failed_structured_result(
            &blobs,
            ToolCallId::new("environment-read"),
            "no_active_environment",
            "No active environment is selected for this session.",
        )
        .await
        .expect("structured failure");

        assert_eq!(result.status, ToolCallStatus::Failed);
        let error = blobs
            .read_text(result.error_ref.as_ref().expect("error ref"))
            .await
            .expect("error json");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&error).expect("json error"),
            serde_json::json!({
                "code": "no_active_environment",
                "message": "No active environment is selected for this session.",
            })
        );
        assert_eq!(
            visible_tool_result_ref(&result),
            result.error_ref.clone().expect("error ref")
        );
    }

    async fn workflow_tool_session(
        blobs: &dyn BlobStore,
        sessions: &InMemorySessionStore,
    ) -> (SessionId, engine::WorkflowToolBinding) {
        let session_id = SessionId::new("workflow-tool-session");
        let schema_ref = blobs
            .put_bytes(
                br#"{"type":"object","properties":{"status":{"type":"string"}},"required":["status"],"additionalProperties":false}"#
                    .to_vec(),
            )
            .await
            .expect("put schema");
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("report"),
            revision: 1,
            semantic_type: "lightspeed.work.report.v1".to_owned(),
            tool: ToolSpec {
                name: ToolName::new("work_report"),
                execution: Default::default(),
                kind: ToolKind::Function(FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: schema_ref,
                    output_schema_ref: None,
                    strict: Some(true),
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
            },
        };
        let receiver = WorkflowEndpointRef {
            workflow_id: "opaque work workflow id".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        };
        let workflow_tools = engine::ManagedSessionWorkflowTools::v1(
            Some(receiver.clone()),
            vec![engine::WorkflowToolDeclaration::bound_notify(
                definition.clone(),
                receiver,
            )],
        );
        let universe_id = uuid::Uuid::from_u128(1);
        let binding = workflow_tools
            .admit(universe_id)
            .expect("admit managed-session tools")
            .bindings
            .into_iter()
            .next()
            .expect("binding");
        sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                display_name: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let config = crate::worker::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "test".to_owned(),
            model: "test-model".to_owned(),
        });
        let proposals = engine::admit_command(
            &engine::CoreAgentState::new(),
            engine::CoreAgentCommand::OpenManagedSession {
                config,
                session_universe_id: universe_id,
                workflow_tools,
            },
            2,
        )
        .expect("open managed session");
        let events = proposals
            .into_iter()
            .map(|proposal| {
                engine::CoreAgentCodec
                    .encode_uncommitted(&proposal.into_uncommitted(2))
                    .expect("encode opening event")
            })
            .collect();
        sessions
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events,
            })
            .await
            .expect("append opening events");
        (session_id, binding)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workflow_tool_calls_validate_schema_ack_and_per_run_cap() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let (session_id, binding) = workflow_tool_session(blobs.as_ref(), sessions.as_ref()).await;
        let valid_arguments = blobs
            .put_bytes(br#"{"status":"complete"}"#.to_vec())
            .await
            .expect("put arguments");
        let invalid_arguments = blobs
            .put_bytes(br#"{"status":4}"#.to_vec())
            .await
            .expect("put invalid arguments");
        let prior_emission_count = 2;
        let tools = SessionTools::new(blobs.clone(), catalog);
        let mut corrupt_binding = binding.clone();
        corrupt_binding.binding_fingerprint.push_str("-corrupt");
        let mut calls = vec![engine::ToolInvocationRequest {
            call_id: ToolCallId::new("call-invalid-schema"),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: invalid_arguments,
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                binding.clone(),
                prior_emission_count,
            )),
            promise_control: None,
        }];
        calls.push(engine::ToolInvocationRequest {
            call_id: ToolCallId::new("call-name-mismatch"),
            tool_name: ToolName::new("other_tool"),
            arguments_ref: valid_arguments.clone(),
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                binding.clone(),
                prior_emission_count,
            )),
            promise_control: None,
        });
        calls.push(engine::ToolInvocationRequest {
            call_id: ToolCallId::new("call-fingerprint-mismatch"),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: valid_arguments.clone(),
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                corrupt_binding,
                prior_emission_count,
            )),
            promise_control: None,
        });
        calls.push(engine::ToolInvocationRequest {
            call_id: ToolCallId::new("call-missing-runtime"),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: valid_arguments.clone(),
            workflow_tool: None,
            promise_control: None,
        });
        calls.extend(
            (0..engine::MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN - prior_emission_count).map(|index| {
                engine::ToolInvocationRequest {
                    call_id: ToolCallId::new(format!("call-{index}")),
                    tool_name: binding.definition.tool.name.clone(),
                    arguments_ref: valid_arguments.clone(),
                    workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                        binding.clone(),
                        prior_emission_count,
                    )),
                    promise_control: None,
                }
            }),
        );
        calls.push(engine::ToolInvocationRequest {
            call_id: ToolCallId::new("call-over-cap"),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: valid_arguments,
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                binding.clone(),
                prior_emission_count,
            )),
            promise_control: None,
        });
        let request = ToolInvocationBatchRequest {
            session_id,
            run_id: RunId::new(9),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            active_environment_id: None,
            environment_policy: None,
            fleet_policy: None,
            workspace_links: Vec::new(),
            calls,
        };
        let retry_request = request.clone();
        let result = tools
            .invoke_batch(request)
            .await
            .expect("invoke workflow tools")
            .completed_result()
            .expect("completed batch");

        let successful = result
            .results
            .iter()
            .filter(|result| result.status == ToolCallStatus::Succeeded)
            .collect::<Vec<_>>();
        assert_eq!(
            successful.len(),
            (engine::MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN - prior_emission_count) as usize
        );
        assert!(successful.iter().all(|result| {
            result.effects.len() == 1
                && result.effects[0].kind == engine::WORKFLOW_TOOL_EMIT_EFFECT_KIND
        }));
        let acknowledgement = blobs
            .read_text(
                successful[0]
                    .output_ref
                    .as_ref()
                    .expect("acknowledgement ref"),
            )
            .await
            .expect("read acknowledgement");
        assert!(acknowledgement.contains("\"accepted\":true"));

        let over_cap = result
            .results
            .iter()
            .find(|result| result.call_id.as_str() == "call-over-cap")
            .expect("cap result");
        let invalid = result
            .results
            .iter()
            .find(|result| result.call_id.as_str() == "call-invalid-schema")
            .expect("schema result");
        let name_mismatch = result
            .results
            .iter()
            .find(|result| result.call_id.as_str() == "call-name-mismatch")
            .expect("name mismatch result");
        let fingerprint_mismatch = result
            .results
            .iter()
            .find(|result| result.call_id.as_str() == "call-fingerprint-mismatch")
            .expect("fingerprint mismatch result");
        let missing_runtime = result
            .results
            .iter()
            .find(|result| result.call_id.as_str() == "call-missing-runtime")
            .expect("missing runtime result");
        assert_eq!(over_cap.status, ToolCallStatus::Failed);
        assert_eq!(invalid.status, ToolCallStatus::Failed);
        assert_eq!(name_mismatch.status, ToolCallStatus::Failed);
        assert_eq!(fingerprint_mismatch.status, ToolCallStatus::Failed);
        assert_eq!(missing_runtime.status, ToolCallStatus::Failed);
        assert!(over_cap.effects.is_empty());
        assert!(invalid.effects.is_empty());
        assert!(name_mismatch.effects.is_empty());
        assert!(fingerprint_mismatch.effects.is_empty());
        assert!(missing_runtime.effects.is_empty());

        sessions
            .create_session(CreateSession {
                session_id: SessionId::new("unrelated-session-created-after-scheduling"),
                display_name: None,
                created_at_ms: 10,
            })
            .await
            .expect("mutate unrelated session-store state");
        let retried = tools
            .invoke_batch(retry_request)
            .await
            .expect("retry workflow tools")
            .completed_result()
            .expect("completed retry");
        assert_eq!(retried, result);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_submit_pins_active_environment_and_provider_policy_in_opaque_context() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let schema_ref = blobs
            .put_bytes(
                br#"{"type":"object","properties":{"jobs":{"type":"array","items":{"type":"object"}}},"required":["jobs"],"additionalProperties":false}"#
                    .to_vec(),
            )
            .await
            .expect("put job schema");
        let recipe = b"test environment job recipe".to_vec();
        let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe);
        let recipe_ref = blobs.put_bytes(recipe).await.expect("put job recipe");
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new(JOB_SUBMIT_WORKFLOW_TOOL_ID),
            revision: 1,
            semantic_type: JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE.to_owned(),
            tool: ToolSpec {
                name: ToolName::new(tools::environment::jobs::JOB_SUBMIT_TOOL_NAME),
                execution: Default::default(),
                kind: ToolKind::Function(FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: schema_ref,
                    output_schema_ref: None,
                    strict: Some(true),
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
            },
        };
        let binding = engine::WorkflowToolBinding::admit(
            uuid::Uuid::from_u128(1),
            definition,
            engine::WorkflowToolTarget::Start {
                start: engine::WorkflowStartRef {
                    recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                    revision: 1,
                    recipe_ref,
                    recipe_fingerprint,
                },
            },
            engine::WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: None,
                max_promises: engine::MAX_COMPLETION_PROMISES,
                key_source: engine::WorkflowToolCompletionKeySource::ArrayIndices {
                    pointer: "/jobs".to_owned(),
                    prefix: "job-".to_owned(),
                },
            },
        )
        .expect("admit environment job binding");
        let arguments_ref = blobs
            .put_bytes(br#"{"jobs":[{"job_id":"build","argv":["make"]}]}"#.to_vec())
            .await
            .expect("put job arguments");
        let call = engine::ToolInvocationRequest {
            call_id: ToolCallId::new("call-job-start"),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: arguments_ref.clone(),
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(binding.clone(), 0)),
            promise_control: None,
        };
        let request = ToolInvocationBatchRequest {
            session_id: SessionId::new("session-job-start"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            active_environment_id: Some(EnvironmentId::new("environment-original")),
            environment_policy: Some(engine::EnvironmentPolicyRuntime::v1(Some(vec![
                "provider-b".to_owned(),
                "provider-a".to_owned(),
            ]))),
            fleet_policy: None,
            workspace_links: Vec::new(),
            calls: vec![call.clone()],
        };
        let tools = SessionTools::new(blobs.clone(), catalog);

        let first = tools
            .invoke_batch(request.clone())
            .await
            .expect("invoke job_submit")
            .completed_result()
            .expect("completed job_submit");
        assert_eq!(first.results[0].status, ToolCallStatus::Succeeded);
        let effect = &first.results[0].effects[0];
        assert_eq!(
            effect.data.get("arguments_ref").map(String::as_str),
            Some(arguments_ref.as_str())
        );
        let context_ref = BlobRef::parse(
            effect
                .data
                .get("execution_context_ref")
                .expect("execution context ref")
                .clone(),
        )
        .expect("valid execution context ref");
        let context: JobSubmitExecutionContextV1 = serde_json::from_slice(
            &blobs
                .read_bytes(&context_ref)
                .await
                .expect("read execution context"),
        )
        .expect("decode execution context");
        assert_eq!(context.environment_id, "environment-original");
        assert_eq!(
            context.allowed_provider_ids,
            Some(vec!["provider-a".to_owned(), "provider-b".to_owned()])
        );
        let retried = tools
            .invoke_batch(request)
            .await
            .expect("retry job_submit")
            .completed_result()
            .expect("completed retry");
        assert_eq!(retried, first);

        let missing_active = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session-job-start"),
                run_id: RunId::new(2),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                active_environment_id: None,
                environment_policy: Some(engine::EnvironmentPolicyRuntime::v1(None)),
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![call],
            })
            .await
            .expect("invoke job_submit without active environment")
            .completed_result()
            .expect("completed missing-active call");
        assert_eq!(missing_active.results[0].status, ToolCallStatus::Failed);
        assert!(missing_active.results[0].effects.is_empty());
        let error = blobs
            .read_text(
                missing_active.results[0]
                    .error_ref
                    .as_ref()
                    .expect("missing-active error"),
            )
            .await
            .expect("read missing-active error");
        assert!(error.contains("requires an active environment"));
    }

    #[derive(Default)]
    struct TestCatalog {
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
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
    }

    fn fleet_test_session(session_id: &SessionId, status: api::SessionStatus) -> api::SessionView {
        api::SessionView {
            id: session_id.as_str().to_owned(),
            status,
            display_name: None,
            managed: false,
            config_revision: 0,
            config: None,
            active_environment_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            runs: Vec::new(),
            active_context: api::ContextView::default(),
            active_tools: api::ActiveToolsView::default(),
            management: None,
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
                orphaned_descendants: false,
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

    async fn session_tools_with_readme_link() -> (
        Arc<InMemoryBlobStore>,
        SessionTools,
        SessionId,
        Vec<WorkspaceLink>,
    ) {
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
        let workspace_links = vec![WorkspaceLink {
            path: "/workspace".to_owned(),
            target: WorkspaceLinkTarget::Workspace {
                workspace_id: workspace_id.to_string(),
            },
            access: WorkspaceLinkAccess::ReadWrite,
        }];
        let tools = SessionTools::new(blobs.clone(), catalog);
        (blobs, tools, session_id, workspace_links)
    }

    fn test_environment(
        blobs: Arc<InMemoryBlobStore>,
        process: Arc<RecordingProcessExecutor>,
    ) -> RuntimeEnvironment {
        let target_id = HostTargetId::new("test");
        let resource = environments::EnvironmentRecord {
            environment_id: engine::EnvironmentId::new("test"),
            request_id: EnvironmentProvisionRequestId::new("request-test"),
            source: EnvironmentSource::Provisioned {
                provider_id: EnvironmentProviderId::new("test-provider"),
                binding_id: EnvironmentProviderBindingId::new("test-binding"),
            },
            display_name: None,
            status: EnvironmentStatus::Offline,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: EnvironmentIncarnationId::new("incarnation-test"),
                provision_request_id: Some(EnvironmentProvisionRequestId::new("request-test")),
                provider_target_id: Some(target_id.clone()),
                template_id: Some(EnvironmentTemplateId::new("test-template")),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: BTreeMap::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let tool_context = EnvironmentToolContext::new(Some(process), blobs.clone())
            .with_process_cwd(FsPath::new("/workspace").expect("process cwd"));
        let fs_context = tools::fs::FsToolContext::new(
            Arc::new(tools::fs::InMemoryFileSystem::full_access()),
            blobs,
        );
        RuntimeEnvironment::from_resource(resource, tool_context, fs_context)
    }

    async fn register_test_environment_provider(
        store: &InMemoryEnvironmentRegistryStore,
        provider_id: &str,
    ) {
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: EnvironmentProviderId::new(provider_id),
                display_name: None,
                controller_connection: HostControllerConnectionSpec::new(
                    "http://controller.test",
                    HostTransport::Http,
                ),
                metadata: BTreeMap::new(),
                updated_at_ms: 10,
            })
            .await
            .expect("register provider");
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id: store.universe_id(),
                binding_id: EnvironmentProviderBindingId::new(format!("binding-{provider_id}")),
                provider_id: EnvironmentProviderId::new(provider_id),
                status: EnvironmentProviderBindingStatus::Enabled,
                expected_revision: None,
                metadata: BTreeMap::new(),
                updated_at_ms: 10,
            })
            .await
            .expect("register provider binding");
    }

    async fn observe_test_environment(
        store: &InMemoryEnvironmentRegistryStore,
        environment_id: &str,
        provider_id: &str,
        observed_at_ms: i64,
    ) {
        let target_id = HostTargetId::new(format!("target-{environment_id}"));
        let environment_id = EnvironmentId::new(environment_id);
        store
            .create_environment(CreateEnvironment {
                request_id: EnvironmentProvisionRequestId::new(format!("request-{environment_id}")),
                environment_id: environment_id.clone(),
                incarnation_id: EnvironmentIncarnationId::new(format!(
                    "incarnation-{environment_id}"
                )),
                binding_id: EnvironmentProviderBindingId::new(format!("binding-{provider_id}")),
                template_id: EnvironmentTemplateId::new("test-template"),
                display_name: None,
                metadata: BTreeMap::new(),
                created_at_ms: observed_at_ms.saturating_sub(1),
            })
            .await
            .expect("create environment");
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id,
                provider_target_id: target_id,
                status: EnvironmentStatus::Offline,
                observed_at_ms,
            })
            .await
            .expect("observe environment");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_call_executes_one_environment_control_call() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let registry = Arc::new(InMemoryEnvironmentRegistryStore::new());
        register_test_environment_provider(registry.as_ref(), "allowed").await;
        observe_test_environment(registry.as_ref(), "environment-allowed-1", "allowed", 10).await;
        let resolver = crate::environment_resolver::EnvironmentResolver::new(
            registry.clone(),
            registry.clone(),
        );
        let tools = SessionTools::new(blobs.clone(), catalog).with_environment_resolver(resolver);
        let arguments_ref = blobs
            .put_bytes(br#"{}"#.to_vec())
            .await
            .expect("list arguments");
        let request = engine::ToolInvocationCallRequest {
            session_id: SessionId::new("session-per-call-list"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            workspace_links: Vec::new(),
            active_environment_id: Some(EnvironmentId::new("environment-allowed-1")),
            environment_policy: Some(engine::EnvironmentPolicyRuntime::v1(Some(vec![
                "allowed".to_owned(),
            ]))),
            fleet_policy: None,
            call: engine::ToolInvocationRequest {
                call_id: ToolCallId::new("call-environment-list"),
                tool_name: ToolName::new(ENVIRONMENT_LIST_TOOL_NAME),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
            },
            sibling_calls: Vec::new(),
            execution: engine::ToolExecutionSpec::default(),
        };

        let result = tools.invoke_call(request).await.expect("invoke call");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let output: serde_json::Value = serde_json::from_slice(
            &blobs
                .read_bytes(result.output_ref.as_ref().expect("output"))
                .await
                .expect("read output"),
        )
        .expect("decode output");
        assert_eq!(
            output["environments"][0]["environment_id"],
            "environment-allowed-1"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_call_rejects_batch_unit_tools() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let tools = SessionTools::new(blobs.clone(), Arc::new(TestCatalog::default()));
        let arguments_ref = blobs
            .put_bytes(br#"{}"#.to_vec())
            .await
            .expect("await arguments");
        let request = engine::ToolInvocationCallRequest {
            session_id: SessionId::new("session-per-call-await"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            fleet_policy: None,
            call: engine::ToolInvocationRequest {
                call_id: ToolCallId::new("call-await"),
                tool_name: ToolName::new(AWAIT_TOOL_NAME),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
            },
            sibling_calls: Vec::new(),
            execution: engine::ToolExecutionSpec::default(),
        };

        let result = tools.invoke_call(request).await.expect("invoke call");

        assert_eq!(result.status, ToolCallStatus::Failed);
        let error = blobs
            .read_text(result.error_ref.as_ref().expect("error ref"))
            .await
            .expect("error text");
        assert!(error.contains("batch-unit execution"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn environment_list_uses_supplied_policy_and_live_resolver_state_without_session_store() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let registry = Arc::new(InMemoryEnvironmentRegistryStore::new());
        register_test_environment_provider(registry.as_ref(), "allowed").await;
        register_test_environment_provider(registry.as_ref(), "denied").await;
        observe_test_environment(registry.as_ref(), "environment-allowed-1", "allowed", 10).await;
        observe_test_environment(registry.as_ref(), "environment-denied", "denied", 10).await;
        let resolver = crate::environment_resolver::EnvironmentResolver::new(
            registry.clone(),
            registry.clone(),
        );
        let tools = SessionTools::new(blobs.clone(), catalog).with_environment_resolver(resolver);
        let arguments_ref = blobs
            .put_bytes(br#"{}"#.to_vec())
            .await
            .expect("list arguments");
        let request = ToolInvocationBatchRequest {
            session_id: SessionId::new("session-environment-list"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            active_environment_id: Some(EnvironmentId::new("environment-allowed-1")),
            environment_policy: Some(engine::EnvironmentPolicyRuntime::v1(Some(vec![
                "allowed".to_owned(),
            ]))),
            fleet_policy: None,
            workspace_links: Vec::new(),
            calls: vec![engine::ToolInvocationRequest {
                call_id: ToolCallId::new("call-environment-list"),
                tool_name: ToolName::new(ENVIRONMENT_LIST_TOOL_NAME),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
            }],
        };

        let first = tools
            .invoke_batch(request.clone())
            .await
            .expect("first list")
            .completed_result()
            .expect("completed list");
        let first_output: serde_json::Value = serde_json::from_slice(
            &blobs
                .read_bytes(first.results[0].output_ref.as_ref().expect("first output"))
                .await
                .expect("read first output"),
        )
        .expect("decode first output");
        let first_environments = first_output["environments"]
            .as_array()
            .expect("first environments");
        assert_eq!(first_environments.len(), 1);
        assert_eq!(
            first_environments[0]["environment_id"],
            "environment-allowed-1"
        );
        assert_eq!(first_environments[0]["active"], true);

        observe_test_environment(registry.as_ref(), "environment-allowed-2", "allowed", 20).await;
        let second = tools
            .invoke_batch(request)
            .await
            .expect("second list")
            .completed_result()
            .expect("completed second list");
        let second_output: serde_json::Value = serde_json::from_slice(
            &blobs
                .read_bytes(
                    second.results[0]
                        .output_ref
                        .as_ref()
                        .expect("second output"),
                )
                .await
                .expect("read second output"),
        )
        .expect("decode second output");
        assert_eq!(
            second_output["environments"]
                .as_array()
                .expect("second environments")
                .len(),
            2
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_read_vfs_workspace_link() {
        let (blobs, tools, session_id, workspace_links) = session_tools_with_readme_link().await;
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links,
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("vfs_read_file"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
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
    async fn session_tools_accept_claude_style_vfs_read_tool() {
        let (blobs, tools, session_id, workspace_links) = session_tools_with_readme_link().await;
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links,
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("VfsRead"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
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
    async fn session_tools_route_vfs_file_tools_and_environment_process_tools_separately() {
        let (blobs, tools, session_id, workspace_links) = session_tools_with_readme_link().await;
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
                active_environment_id: Some(EnvironmentId::new("test")),
                environment_policy: Some(engine::EnvironmentPolicyRuntime::v1(None)),
                fleet_policy: None,
                workspace_links,
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_read"),
                        tool_name: ToolName::new("vfs_read_file"),
                        arguments_ref: read_args,
                        workflow_tool: None,
                        promise_control: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_process"),
                        tool_name: ToolName::new("exec_command"),
                        arguments_ref: process_args,
                        workflow_tool: None,
                        promise_control: None,
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
        let mut config = crate::worker::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "test".to_owned(),
            model: "test-model".to_owned(),
        });
        // Fleet spawning requires the fleet feature grant on the parent.
        config.features.fleet = Some(engine::FleetFeature::default());
        state.lifecycle.config = Some(config);
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
        let tools = SessionTools::new(blobs.clone(), catalog.clone())
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: Some(engine::FleetFeature::default()),
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_spawn"),
                    tool_name: ToolName::new(::tools::fleet::AGENT_SPAWN_TOOL_NAME),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
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
        let tools = SessionTools::new(blobs.clone(), catalog.clone())
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_send_1"),
                        tool_name: ToolName::new(::tools::fleet::AGENT_SEND_TOOL_NAME),
                        arguments_ref: arguments_ref.clone(),
                        workflow_tool: None,
                        promise_control: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_send_2"),
                        tool_name: ToolName::new(::tools::fleet::AGENT_SEND_TOOL_NAME),
                        arguments_ref,
                        workflow_tool: None,
                        promise_control: None,
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
                            ownership: engine::PromiseOwnership::Model,
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
        let tools = SessionTools::new(blobs.clone(), catalog.clone())
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_wait"),
                        tool_name: ToolName::new(::tools::concurrency::AWAIT_TOOL_NAME),
                        arguments_ref: wait_args,
                        workflow_tool: None,
                        promise_control: None,
                    },
                    engine::ToolInvocationRequest {
                        call_id: ToolCallId::new("call_read"),
                        tool_name: ToolName::new("read_file"),
                        arguments_ref: read_args,
                        workflow_tool: None,
                        promise_control: None,
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
                                    ownership: engine::PromiseOwnership::Model,
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
        let tools = SessionTools::new(blobs.clone(), catalog.clone());
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_wait"),
                    tool_name: ToolName::new(::tools::concurrency::AWAIT_TOOL_NAME),
                    arguments_ref: wait_args,
                    workflow_tool: None,
                    promise_control: None,
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
    async fn cancel_uses_supplied_runtime_without_session_store() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let parent = SessionId::new("parent_no_fleet_cancel");
        let tools = SessionTools::new(blobs.clone(), catalog.clone());
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_cancel"),
                    tool_name: ToolName::new(::tools::concurrency::CANCEL_TOOL_NAME),
                    arguments_ref: cancel_args,
                    workflow_tool: None,
                    promise_control: Some(engine::PromiseControlCallRuntime::v1(vec![
                        engine::PromiseControlRuntime {
                            promise_id: engine::PromiseId::new("promise_job"),
                            state: engine::PromiseControlStateRuntime::Known {
                                ownership: engine::PromiseOwnership::Model,
                                scope: engine::PromiseScope::Session,
                                promise_status: engine::PromiseStatus::Pending,
                            },
                        },
                    ])),
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
    async fn detach_uses_supplied_runtime_without_session_store() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let parent = SessionId::new("parent_no_fleet_detach");
        let tools = SessionTools::new(blobs.clone(), catalog.clone());
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_detach"),
                    tool_name: ToolName::new(::tools::concurrency::DETACH_TOOL_NAME),
                    arguments_ref: detach_args,
                    workflow_tool: None,
                    promise_control: Some(engine::PromiseControlCallRuntime::v1(vec![
                        engine::PromiseControlRuntime {
                            promise_id: engine::PromiseId::new("promise_job"),
                            state: engine::PromiseControlStateRuntime::Known {
                                ownership: engine::PromiseOwnership::Model,
                                scope: engine::PromiseScope::Run {
                                    run_id: RunId::new(1),
                                },
                                promise_status: engine::PromiseStatus::Pending,
                            },
                        },
                    ])),
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
        let tools = SessionTools::new(blobs.clone(), catalog);
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_sleep"),
                    tool_name: ToolName::new(::tools::concurrency::SLEEP_TOOL_NAME),
                    arguments_ref: sleep_args,
                    workflow_tool: None,
                    promise_control: None,
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
    async fn session_tools_fail_vfs_tool_without_workspace_links() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog);
        let arguments_ref = BlobRef::from_bytes(b"{}");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("vfs_read_file"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
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
        assert!(error.contains("no_vfs_workspace_links"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_fetch_runs_without_filesystem_domains() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog);
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
                active_environment_id: None,
                environment_policy: None,
                fleet_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new("web_fetch"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
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
        assert!(!error.contains("no_vfs_workspace_links"));
    }
}
