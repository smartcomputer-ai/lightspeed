use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use engine::PromiseIdAllocator;
use engine::{
    CoreAgentIoError, CoreAgentTools, PromiseSource, SessionId, ToolBatchOutcome, ToolCallStatus,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    promise_create_effect,
    storage::{BlobEdge, BlobGraphStore, BlobStore, BlobStoreError},
};
use environment_client::{EnvironmentClientError, EnvironmentDataClient, WebSocketConnectOptions};
use environment_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        jobs::{JobReadResult as ProtocolJobReadResult, ReadJobsParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, EnvironmentDataConnection, EnvironmentTransport},
};
use environments::{
    EnvironmentAccessPolicy, EnvironmentId, EnvironmentRecord, EnvironmentRegistryError,
    EnvironmentStore,
};
use store_pg::PgStore;
use tools::{
    builtin::BuiltinToolRequirements,
    concurrency::{
        AwaitArgs, CancelArgs, DetachArgs, SleepArgs, SleepOutput, cancel_promises_from_runtime,
        cancel_promises_model_visible_text, detach_promises_from_runtime,
        detach_promises_model_visible_text, is_concurrency_tool, sleep_model_visible_text,
    },
    environment::control::{
        DEFAULT_ENVIRONMENT_LIST_LIMIT, EnvironmentActivateArgs, EnvironmentDeactivateArgs,
        EnvironmentListArgs, EnvironmentReadArgs, MAX_ENVIRONMENT_LIST_LIMIT,
        is_environment_control_tool, is_environment_selection_tool,
    },
    environment::jobs::{
        JOB_RUN_WORKFLOW_SEMANTIC_TYPE, JOB_RUN_WORKFLOW_TOOL_ID,
        JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE, JOB_SUBMIT_WORKFLOW_TOOL_ID, JobHandle, JobHandleArg,
        JobReadArgs, JobSubmitExecutionContextV1, ModelJobResult, ModelJobResultSet,
        NormalizeJobResultInput, normalize_job_result,
    },
    environment_protocol::RemoteEnvironmentConnection,
    fs::{FsPath, FsToolContext, LinkedVfsFileSystem},
    limits::ToolLimits,
    runtime::InlineToolRuntime,
    runtime::ToolCatalog,
    subagents::{AgentCallArgs, SubagentExecutionContextV1, SubagentToolKind},
    workflow_tool::invoke_workflow_tool,
};
use vfs::{ResolvedWorkspaceLink, VfsCatalogError, VfsWorkspaceStore};

use crate::{
    credential_injection::EnvironmentCredentialResolver,
    environment::{ActiveEnvironmentBlocker, RuntimeEnvironment, SessionEnvironmentManager},
    subagents::await_spec_from_args,
};

#[derive(Clone)]
pub struct SessionTools {
    blobs: Arc<dyn BlobStore>,
    blob_graph: Option<Arc<dyn BlobGraphStore>>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    environments: SessionEnvironmentManager,
    environment_store: Option<Arc<dyn EnvironmentStore>>,
    registration_keys: Option<Arc<dyn environments::EnvironmentRegistrationKeyStore>>,
    environment_resolver: Option<crate::environment_resolver::EnvironmentResolver>,
    environment_credentials: Option<EnvironmentCredentialResolver>,
    environment_gateway: Option<crate::environment_gateway::EnvironmentGatewayClientConfig>,
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
            registration_keys: None,
            environment_resolver: None,
            environment_credentials: None,
            environment_gateway: None,
        }
    }

    pub fn with_environment_store(mut self, environments: Arc<dyn EnvironmentStore>) -> Self {
        self.environment_store = Some(environments);
        self
    }

    /// Registration keys name the groups registered environments belong to;
    /// the model sees the group name, never the key.
    pub fn with_registration_key_store(
        mut self,
        registration_keys: Arc<dyn environments::EnvironmentRegistrationKeyStore>,
    ) -> Self {
        self.registration_keys = Some(registration_keys);
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

    /// Route environment calls through this deployment's gateway.
    pub fn with_environment_gateway(
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
        let registration_keys: Arc<dyn environments::EnvironmentRegistrationKeyStore> =
            store.clone();
        let credentials = EnvironmentCredentialResolver::from_pg_store(store.clone());
        let resolver =
            crate::environment_resolver::EnvironmentResolver::from_pg_store(store.clone());
        Self::new(blobs, workspace_store)
            .with_blob_graph(blob_graph)
            .with_environment_store(environments)
            .with_registration_key_store(registration_keys)
            .with_environment_resolver(resolver)
            .with_environment_credentials(credentials)
    }

    fn with_blob_graph(mut self, blob_graph: Arc<dyn BlobGraphStore>) -> Self {
        self.blob_graph = Some(blob_graph);
        self
    }

    async fn invoke_concurrency_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        promise_ids: &PromiseIdAllocator,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_id.as_ref().map(|id| id.as_str()) {
            Some("concurrency.cancel") => self.invoke_cancel_call(call).await,
            Some("concurrency.detach") => self.invoke_detach_call(request.run_id, call).await,
            Some("concurrency.sleep") => self.invoke_sleep_call(call, promise_ids).await,
            Some("concurrency.await") => {
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
                    format!("unknown concurrency tool {other:?}"),
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
        call: &engine::ToolInvocationRequest,
        promise_ids: &PromiseIdAllocator,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: SleepArgs = self.read_tool_args(call).await?;
        let fire_at_ms = now_unix_ms()?.saturating_add(args.ms);
        let promise_id = promise_ids.allocate();
        let output = SleepOutput {
            promise: promise_id.to_string(),
            fire_at_ms,
        };
        let visible = sleep_model_visible_text(&output, args.ms);
        let mut result = self.succeeded_tool_result(call, &output, visible).await?;
        result.effects = vec![promise_create_effect(
            &promise_id,
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
        self.invoke_store_backed_await_batch(request, &call).await
    }

    async fn invoke_store_backed_await_batch(
        &self,
        request: ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let args: AwaitArgs = self.read_tool_args(call).await?;
        match await_spec_from_args(args, now_unix_ms()?).map_err(io_error) {
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

    async fn invoke_mixed_await_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let await_calls = request
            .calls
            .iter()
            .filter(|call| {
                call.tool_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "concurrency.await")
            })
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
                .filter(|call| {
                    !call
                        .tool_id
                        .as_ref()
                        .is_some_and(|id| id.as_str() == "concurrency.await")
                })
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
            // Native MCP calls never reach this runtime; the activity
            // wrapper dispatches them and owns approval gating.
            ToolBatchOutcome::AwaitingApproval { .. } => {
                return Err(io_error(
                    "tool runtime reported an approval outcome for a batch without native MCP calls",
                ));
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
            ToolBatchOutcome::AwaitingApproval { .. } => Err(io_error(
                "tool runtime reported an approval outcome for a batch without native MCP calls",
            )),
        }
    }

    async fn invoke_environment_job_call(
        &self,
        request: &ToolInvocationBatchRequest,
        call: &engine::ToolInvocationRequest,
        environments: &SessionEnvironmentManager,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_id.as_ref().map(|id| id.as_str()) {
            Some("env.job_read") => {
                let args: JobReadArgs = self.read_tool_args(call).await?;
                let result = self
                    .read_environment_jobs(
                        &request.session_id,
                        request.active_environment_id.as_ref(),
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

    #[allow(clippy::too_many_arguments)]
    async fn read_environment_jobs(
        &self,
        session_id: &SessionId,
        active_environment_id: Option<&EnvironmentId>,
        environments: &SessionEnvironmentManager,
        handles: Vec<JobHandleArg>,
        output_bytes: Option<usize>,
        after_seq: Option<u64>,
        include_artifacts: bool,
    ) -> Result<EnvironmentJobRead, CoreAgentIoError> {
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let resolved = match resolve_job_handle_arg(active_environment_id, handle) {
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
            let (environment, close_after_read) = if let Some(environment) =
                environments.environment(environment_id.as_str()).cloned()
            {
                (Ok(environment), false)
            } else if let Some(store) = self.environment_store.as_ref() {
                (
                    match store.read_environment(&environment_id).await {
                        Ok(resource) => {
                            self.runtime_environment_for_resource(session_id, resource, None)
                                .await
                        }
                        Err(error) => Err(map_environments_error(error)),
                    },
                    true,
                )
            } else {
                (
                    Err(io_error(
                        "environment store is not configured on this runtime",
                    )),
                    false,
                )
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
                if close_after_read {
                    environment.close().await;
                }
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
            if close_after_read {
                environment.close().await;
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
        promise_ids: &PromiseIdAllocator,
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
            let policy = supplied_environment_policy(request)?;
            let mut context = JobSubmitExecutionContextV1::new(
                environment_id.as_str().to_owned(),
                policy.providers.map(|ids| ids.into_iter().collect()),
                policy
                    .registration_keys
                    .map(|ids| ids.into_iter().collect()),
            );
            context.working_directory = request
                .environment_policy
                .as_ref()
                .and_then(|policy| policy.working_directory.clone());
            Some(
                self.blobs
                    .put_bytes(serde_json::to_vec(&context).map_err(io_error)?)
                    .await
                    .map_err(map_blob_error)?,
            )
        } else if SubagentToolKind::from_binding(
            binding.definition.tool_id.as_str(),
            binding.definition.semantic_type.as_str(),
        )
        .is_some()
        {
            // Sub-agent admission: the grant on the batch request is
            // the authority. Validate the agent against its allowlist here,
            // pin the grant limits and parent identity for the execution,
            // and let the generic start-on-call path do the rest.
            let Some(policy) = request.subagents_policy.as_ref() else {
                return failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!(
                        "{} requires the subagents grant",
                        binding.definition.tool.name
                    ),
                )
                .await;
            };
            let args: AgentCallArgs = match self.read_tool_args(call).await {
                Ok(args) => args,
                Err(error) => {
                    return failed_result(
                        self.blobs.as_ref(),
                        call.call_id.clone(),
                        error.to_string(),
                    )
                    .await;
                }
            };
            if let Err(error) = args.validate() {
                return failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await;
            }
            if !policy.agent_allowed(&args.agent) {
                let allowed = policy
                    .agents
                    .iter()
                    .map(|agent| agent.profile_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!(
                        "agent {} is not in this session's sub-agent catalog (allowed: {allowed})",
                        args.agent
                    ),
                )
                .await;
            }
            let context = SubagentExecutionContextV1::new(
                request.session_id.as_str().to_owned(),
                request.run_id.as_u64(),
                args.agent,
                policy.limits,
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
            promise_ids,
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
        promise_ids: &PromiseIdAllocator,
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
            .invoke_workflow_tool_call(request, call, &runtime.binding, emitted_count, promise_ids)
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
        let visible = visible.into().into_bytes();
        let output_bytes = visible.len() as u64;
        let visible_ref = self
            .blobs
            .put_bytes(visible)
            .await
            .map_err(map_blob_error)?;
        Ok(ToolInvocationResult {
            duration_ms: None,
            output_bytes: Some(output_bytes),
            truncated: false,
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
        let output_json = serde_json::to_vec(&output).map_err(io_error)?;
        let output_bytes = output_json.len() as u64;
        let output_ref = self
            .blobs
            .put_bytes(output_json)
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
            duration_ms: None,
            output_bytes: Some(output_bytes),
            truncated: false,
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
        match call.tool_id.as_ref().map(|id| id.as_str()) {
            Some("environment.list") => {
                let args: EnvironmentListArgs = self.read_tool_args(call).await?;
                let limit = args
                    .limit
                    .unwrap_or(DEFAULT_ENVIRONMENT_LIST_LIMIT)
                    .clamp(1, MAX_ENVIRONMENT_LIST_LIMIT);
                let mut environments = match resolver.list_allowed(&allowed).await {
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
                let groups = self.environment_groups(&environments).await;
                if let Some(group) = args.group.as_deref() {
                    environments.retain(|environment| {
                        group_of(environment, &groups)
                            .is_some_and(|name| name.eq_ignore_ascii_case(group))
                    });
                }
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
                        environment_model_view(environment, active, group_of(environment, &groups))
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
            Some("environment.read") => {
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
                let environment = match resolver.read_allowed(&environment_id, &allowed).await {
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
                let groups = self
                    .environment_groups(std::slice::from_ref(&environment))
                    .await;
                let mut output =
                    environment_model_view(&environment, active, group_of(&environment, &groups));
                if crate::environment_resolver::wake_on_use_applies(&environment) {
                    output["status_message"] = serde_json::json!(format!(
                        "Environment is {}. Tools that use this environment will automatically wake it and wait until it is ready. You can proceed normally.",
                        format!("{:?}", environment.status).to_lowercase(),
                    ));
                }
                self.succeeded_tool_result(
                    call,
                    &output,
                    serde_json::to_string_pretty(&output).map_err(io_error)?,
                )
                .await
            }
            Some("environment.activate") => {
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
                let (environment, ready) = match resolver
                    .activatable(
                        &environment_id,
                        &allowed,
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
                    "ready": ready,
                    "status": format!("{:?}", environment.status).to_lowercase(),
                });
                let summary = if ready {
                    format!("Active environment set to {}.", environment.environment_id)
                } else {
                    format!(
                        "Active environment set to {} (still {}; environment tools wait until it is ready).",
                        environment.environment_id,
                        format!("{:?}", environment.status).to_lowercase()
                    )
                };
                let mut result = self.succeeded_tool_result(call, &output, summary).await?;
                result.effects.push(engine::environment_activate_effect(
                    &environment.environment_id,
                ));
                Ok(result)
            }
            Some("environment.deactivate") => {
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
                    format!("unknown environment control tool {other:?}"),
                )
                .await
            }
        }
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
        if let Some(environment) = environments.environment(environment_id.as_str()).cloned() {
            if let Some(cwd) = request
                .environment_policy
                .as_ref()
                .and_then(|policy| policy.working_directory.as_deref())
            {
                let environment = environment
                    .with_working_directory(FsPath::new(cwd).map_err(io_error)?)
                    .await
                    .map_err(io_error)?;
                environments.insert_environment(environment);
            }
            return Ok(environments);
        }
        let resource = if let Some(resolver) = self.environment_resolver.as_ref() {
            match resolver
                .resolve_for_connection(
                    environment_id,
                    &allowed,
                    i64::try_from(now_unix_ms()?)
                        .map_err(|_| io_error("current timestamp does not fit in i64"))?,
                )
                .await
            {
                Ok(resource) => resource,
                Err(crate::environment_resolver::EnvironmentResolveError::Store(
                    environments::EnvironmentRegistryError::Store { message },
                )) => return Err(io_error(message)),
                Err(crate::environment_resolver::EnvironmentResolveError::NotReady {
                    environment_id,
                    status,
                }) => {
                    return Ok(environments.with_active_blocker(
                        ActiveEnvironmentBlocker::NotReady {
                            environment_id,
                            status,
                        },
                    ));
                }
                Err(error) => {
                    return Ok(environments.with_active_blocker(
                        ActiveEnvironmentBlocker::Unavailable {
                            message: error.to_string(),
                        },
                    ));
                }
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
            if !allowed.allows(&resource) {
                return Ok(environments);
            }
            resource
        };
        let environment = match self
            .runtime_environment_for_resource(
                &request.session_id,
                resource,
                request
                    .environment_policy
                    .as_ref()
                    .and_then(|policy| policy.working_directory.as_deref()),
            )
            .await
        {
            Ok(environment) => environment,
            Err(error) => {
                return Ok(environments.with_active_blocker(
                    ActiveEnvironmentBlocker::Unavailable {
                        message: error.to_string(),
                    },
                ));
            }
        };
        environments.insert_environment(environment);
        Ok(environments)
    }

    /// Registration-key display names for the registered environments in a
    /// listing, keyed by key id. A key that fails to load simply yields no
    /// group; the listing itself is never blocked by it.
    async fn environment_groups(
        &self,
        environments: &[EnvironmentRecord],
    ) -> BTreeMap<String, String> {
        let mut groups = BTreeMap::new();
        let Some(registration_keys) = self.registration_keys.as_ref() else {
            return groups;
        };
        for key_id in environments
            .iter()
            .filter_map(|environment| environment.registration_key_id())
        {
            if groups.contains_key(key_id.as_str()) {
                continue;
            }
            if let Ok(key) = registration_keys.read_registration_key(key_id).await {
                groups.insert(key_id.to_string(), key.display_name);
            }
        }
        groups
    }

    async fn runtime_environment_for_resource(
        &self,
        session_id: &SessionId,
        resource: EnvironmentRecord,
        working_directory: Option<&str>,
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
        let mut client = connect_environment_data_client(
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
            .map_err(map_environment_client_error)?;
        if response.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(io_error(format!(
                "unsupported environment data protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                response.protocol_version
            )));
        }
        client
            .initialized(&InitializedParams {})
            .await
            .map_err(map_environment_client_error)?;
        let cwd = if response.capabilities.filesystem_read {
            match crate::environment_sources::working_directory(
                &mut client,
                working_directory,
                response.default_cwd.as_deref(),
            )
            .await
            {
                Ok(cwd) => cwd,
                Err(error) => {
                    let _ = client.close().await;
                    return Err(io_error(error));
                }
            }
        } else {
            let cwd = working_directory
                .or(response.default_cwd.as_deref())
                .ok_or_else(|| io_error("environment has no working directory"))?;
            tools::environment::sources::absolute(std::path::Path::new("/"), cwd)
                .map_err(io_error)?
                .to_string_lossy()
                .into_owned()
        };
        let cwd = Some(FsPath::new(cwd).map_err(io_error)?);

        let mut remote_connection = RemoteEnvironmentConnection::new(client, response.capabilities);
        if let Some(cwd) = cwd {
            remote_connection = remote_connection.with_cwd(cwd);
        }
        let (_fs_context, mut environment_context) =
            remote_connection.clone().into_contexts(self.blobs.clone());
        if let Some(credentials) = &self.environment_credentials {
            environment_context =
                credentials.wrap_context(environment_context, resource.environment_id.clone());
        }
        let environment_context =
            environment_context.with_session_id(session_id.as_str().to_owned());
        Ok(
            RuntimeEnvironment::from_resource(resource, environment_context)
                .with_remote_connection(remote_connection),
        )
    }

    async fn runtime_for_domains(
        &self,
        links: Vec<ResolvedWorkspaceLink>,
        environments: &SessionEnvironmentManager,
        active_environment_id: Option<&EnvironmentId>,
        vfs_working_directory: Option<&str>,
    ) -> Result<InlineToolRuntime, CoreAgentIoError> {
        let vfs = if links.is_empty() {
            None
        } else {
            let fs = LinkedVfsFileSystem::new(
                self.blobs.clone(),
                self.workspace_store.clone(),
                links.clone(),
            )
            .map_err(io_error)?
            .with_blob_graph(self.blob_graph.clone());
            let cwd = FsPath::new(vfs_working_directory.unwrap_or("/")).map_err(io_error)?;
            let metadata = tools::fs::FileSystem::get_metadata(&fs, &cwd)
                .await
                .map_err(io_error)?;
            if !metadata.is_directory {
                return Err(io_error(format!(
                    "VFS working directory is not a directory: {cwd}"
                )));
            }
            Some(FsToolContext::new(Arc::new(fs), self.blobs.clone()).with_cwd(cwd))
        };
        let environment =
            active_environment_id.and_then(|id| environments.active_tool_context(id.as_str()));
        Ok(InlineToolRuntime::with_contexts_and_blob_store(
            vfs,
            environment,
            self.blobs.clone(),
            ToolLimits::default(),
            ToolCatalog::new(),
        ))
    }
}

struct EnvironmentJobRead {
    entries: Vec<ModelJobResult>,
}

fn environment_model_view(
    environment: &EnvironmentRecord,
    active: Option<&EnvironmentId>,
    group: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "environment_id": environment.environment_id.as_str(),
        "provider_id": environment.provider_id().map(|id| id.as_str()),
        "group": group,
        "display_name": environment.display_name,
        "status": format!("{:?}", environment.status).to_lowercase(),
        "active": active == Some(&environment.environment_id),
        "observed_at_ms": environment.observed_at_ms(),
    })
}

/// The group name of a registered environment: its registration key's
/// display name, when the key resolved.
fn group_of<'a>(
    environment: &EnvironmentRecord,
    groups: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    environment
        .registration_key_id()
        .and_then(|id| groups.get(id.as_str()))
        .map(String::as_str)
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

/// Reject ungranted operations before connecting to or waking a machine.
fn environment_tool_denial(
    policy: Option<&engine::EnvironmentPolicyRuntime>,
    call: &engine::ToolInvocationRequest,
) -> Option<&'static str> {
    let id = call.tool_id.as_ref()?.as_str();
    let surface = policy.and_then(|policy| policy.tools);
    match id {
        "env.read_file" | "env.grep" | "env.glob" | "env.list_dir" | "vfs.capture"
            if surface.is_none() =>
        {
            Some("environment filesystem read tools are not granted")
        }
        "env.write_file" | "env.edit_file" | "env.apply_patch" | "vfs.materialize"
            if surface != Some(engine::EnvironmentToolSurface::Edit) =>
        {
            Some("environment filesystem editing is not granted")
        }
        "env.run_process" | "env.continue_process"
            if !policy.is_some_and(|policy| policy.commands) =>
        {
            Some("environment command execution is not granted")
        }
        _ => None,
    }
}

fn supplied_environment_policy(
    request: &ToolInvocationBatchRequest,
) -> Result<EnvironmentAccessPolicy, CoreAgentIoError> {
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
    Ok(access_policy_from_runtime(policy))
}

fn access_policy_from_runtime(
    policy: &engine::EnvironmentPolicyRuntime,
) -> EnvironmentAccessPolicy {
    EnvironmentAccessPolicy::new(
        policy.allowed_provider_ids.clone(),
        policy.allowed_registration_key_ids.clone(),
    )
}

async fn job_read_entry_from_response(
    blobs: &dyn BlobStore,
    handle: JobHandle,
    response: Option<ProtocolJobReadResult>,
    output_bytes: Option<usize>,
) -> Result<ModelJobResult, CoreAgentIoError> {
    match response {
        Some(response) => normalize_job_result(
            blobs,
            NormalizeJobResultInput {
                handle: Some(handle),
                summary: Some(response.summary),
                output_chunks: response.output_chunks,
                output_next_seq: response.output_next_seq,
                artifacts: response.artifacts,
                output_bytes,
                ..Default::default()
            },
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

async fn connect_environment_data_client(
    connection: &EnvironmentDataConnection,
    options: WebSocketConnectOptions,
) -> Result<EnvironmentDataClient<environment_client::WebSocketTransport>, CoreAgentIoError> {
    match &connection.transport {
        EnvironmentTransport::WebSocket => {
            EnvironmentDataClient::connect(&connection.endpoint, options)
                .await
                .map_err(map_environment_client_error)
        }
        EnvironmentTransport::Http => Err(unsupported_environment_data_transport("http")),
        EnvironmentTransport::Stdio => Err(unsupported_environment_data_transport("stdio")),
        EnvironmentTransport::Ssh => Err(unsupported_environment_data_transport("ssh")),
        EnvironmentTransport::Provider { provider_type } => Err(
            unsupported_environment_data_transport(format!("provider:{provider_type}")),
        ),
    }
}

fn unsupported_environment_data_transport(transport: impl std::fmt::Display) -> CoreAgentIoError {
    io_error(format!(
        "environment data transport is not supported by this worker: {transport}"
    ))
}

#[async_trait]
impl CoreAgentTools for SessionTools {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        // One allocator per dispatch: every promise this batch's calls mint
        // is numbered from the engine's base, whichever call draws first.
        let promise_ids = PromiseIdAllocator::new(request.promise_id_base);
        let selection_calls = request
            .calls
            .iter()
            .filter(|call| {
                call.tool_id
                    .as_ref()
                    .is_some_and(is_environment_selection_tool)
            })
            .count();
        let mixes_environment_dependency = selection_calls > 0
            && request.calls.iter().any(|call| {
                !call
                    .tool_id
                    .as_ref()
                    .is_some_and(is_environment_selection_tool)
                    && call
                        .tool_id
                        .as_ref()
                        .is_some_and(|id| BuiltinToolRequirements::for_id(id).active_environment)
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
        let has_await_call = request.calls.iter().any(|call| {
            call.tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "concurrency.await")
        });
        if has_await_call && request.calls.len() == 1 {
            return self.invoke_lone_await_batch(request).await;
        }
        if has_await_call {
            return self.invoke_mixed_await_batch(request).await;
        }
        let has_generic_runtime_call = request.calls.iter().any(|call| {
            !call.tool_id.as_ref().is_some_and(is_concurrency_tool)
                && !call
                    .tool_id
                    .as_ref()
                    .is_some_and(is_environment_control_tool)
                && call.workflow_tool.is_none()
        });
        let mut successful_workflow_siblings = BTreeMap::new();
        if !has_generic_runtime_call {
            // Workflow-tool/concurrency-only batches skip generic VFS/runtime
            // setup entirely.
            let mut results = Vec::with_capacity(request.calls.len());
            for call in &request.calls {
                if let Some(message) =
                    environment_tool_denial(request.environment_policy.as_ref(), call)
                {
                    results.push(
                        failed_result(self.blobs.as_ref(), call.call_id.clone(), message).await?,
                    );
                } else if call.workflow_tool.is_some() {
                    results.push(
                        self.invoke_supplied_workflow_tool_call(
                            &request,
                            call,
                            &mut successful_workflow_siblings,
                            &promise_ids,
                        )
                        .await?,
                    );
                } else if call
                    .tool_id
                    .as_ref()
                    .is_some_and(is_environment_control_tool)
                {
                    results.push(self.invoke_environment_control_call(&request, call).await?);
                } else {
                    results.push(
                        self.invoke_concurrency_call(&request, call, &promise_ids)
                            .await?,
                    );
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
            if environment_tool_denial(request.environment_policy.as_ref(), call).is_some() {
                return false;
            }
            call.tool_id
                .as_ref()
                .is_some_and(|id| BuiltinToolRequirements::for_id(id).vfs)
        });
        let has_environment_call = request.calls.iter().any(|call| {
            if environment_tool_denial(request.environment_policy.as_ref(), call).is_some() {
                return false;
            }
            call.tool_id
                .as_ref()
                .is_some_and(|id| BuiltinToolRequirements::for_id(id).active_environment)
                || call
                    .tool_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "env.job_read")
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
        let outcome = async {
            let runtime = self
                .runtime_for_domains(
                    links,
                    &environments,
                    request.active_environment_id.as_ref(),
                    request.vfs_working_directory.as_deref(),
                )
                .await?;

            let mut results = Vec::with_capacity(request.calls.len());
            for call in &request.calls {
                if let Some(message) =
                    environment_tool_denial(request.environment_policy.as_ref(), call)
                {
                    results.push(
                        failed_result(self.blobs.as_ref(), call.call_id.clone(), message).await?,
                    );
                } else if call.workflow_tool.is_some() {
                    results.push(
                        self.invoke_supplied_workflow_tool_call(
                            &request,
                            call,
                            &mut successful_workflow_siblings,
                            &promise_ids,
                        )
                        .await?,
                    );
                } else if call.tool_id.as_ref().is_some_and(is_concurrency_tool) {
                    results.push(
                        self.invoke_concurrency_call(&request, call, &promise_ids)
                            .await?,
                    );
                } else if call
                    .tool_id
                    .as_ref()
                    .is_some_and(is_environment_control_tool)
                {
                    results.push(self.invoke_environment_control_call(&request, call).await?);
                } else if let Some(blocker) = environments.active_blocker().filter(|_| {
                    call.tool_id
                        .as_ref()
                        .is_some_and(|id| id.as_str() == "env.job_read")
                        || call.tool_id.as_ref().is_some_and(|id| {
                            BuiltinToolRequirements::for_id(id).active_environment
                        })
                }) {
                    // Batch-unit execution has no workflow-level readiness wait;
                    // report the blocker as an ordinary failed call.
                    results.push(
                        failed_result(
                            self.blobs.as_ref(),
                            call.call_id.clone(),
                            active_environment_blocker_message(blocker),
                        )
                        .await?,
                    );
                } else if call
                    .tool_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "env.job_read")
                {
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
        .await;
        environments.close().await;
        outcome
    }

    async fn invoke_call(
        &self,
        request: engine::ToolInvocationCallRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match self.invoke_call_execution(request).await? {
            ToolCallExecution::Completed(result) => Ok(result),
            ToolCallExecution::EnvironmentNotReady {
                call_id,
                environment_id,
                status,
            } => {
                failed_result(
                    self.blobs.as_ref(),
                    call_id,
                    active_environment_blocker_message(&ActiveEnvironmentBlocker::NotReady {
                        environment_id,
                        status,
                    }),
                )
                .await
            }
        }
    }
}

/// Outcome of executing one call on the hosted per-call path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCallExecution {
    Completed(ToolInvocationResult),
    /// The call did not execute because the session's active environment is
    /// still provisioning or booting. The workflow waits for readiness and
    /// re-dispatches the same call.
    EnvironmentNotReady {
        call_id: engine::ToolCallId,
        environment_id: String,
        status: environments::EnvironmentStatus,
    },
}

impl SessionTools {
    /// Per-call execution that distinguishes "did not run because the active
    /// environment is not ready yet" from ordinary results, so the workflow
    /// can wait outside the tool activity's tight class deadline.
    pub async fn invoke_call_execution(
        &self,
        request: engine::ToolInvocationCallRequest,
    ) -> Result<ToolCallExecution, CoreAgentIoError> {
        let call = request.call.clone();
        if let Some(message) = environment_tool_denial(request.environment_policy.as_ref(), &call) {
            return failed_result(self.blobs.as_ref(), call.call_id, message)
                .await
                .map(ToolCallExecution::Completed);
        }
        // Batch-unit tools never arrive here: the workflow routes batches
        // containing them through the batch activity.
        if call.workflow_tool.is_some()
            || call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "concurrency.await")
        {
            return failed_result(
                self.blobs.as_ref(),
                call.call_id,
                "this tool call requires batch-unit execution",
            )
            .await
            .map(ToolCallExecution::Completed);
        }
        if let Some(message) = per_call_batch_rule_violation(&request) {
            return failed_result(self.blobs.as_ref(), call.call_id, message)
                .await
                .map(ToolCallExecution::Completed);
        }
        let batch_request = request.into_batch_request();
        if call.tool_id.as_ref().is_some_and(is_concurrency_tool) {
            // A per-call dispatch owns exactly one promise slot.
            let promise_ids = PromiseIdAllocator::new(batch_request.promise_id_base);
            return self
                .invoke_concurrency_call(&batch_request, &call, &promise_ids)
                .await
                .map(ToolCallExecution::Completed);
        }
        if call
            .tool_id
            .as_ref()
            .is_some_and(is_environment_control_tool)
        {
            return self
                .invoke_environment_control_call(&batch_request, &call)
                .await
                .map(ToolCallExecution::Completed);
        }
        let is_job_call = call
            .tool_id
            .as_ref()
            .is_some_and(|id| id.as_str() == "env.job_read");
        let is_vfs_call = call
            .tool_id
            .as_ref()
            .is_some_and(|id| BuiltinToolRequirements::for_id(id).vfs);
        let is_environment_call = call
            .tool_id
            .as_ref()
            .is_some_and(|id| BuiltinToolRequirements::for_id(id).active_environment);
        let environments = if is_environment_call || is_job_call {
            let environments = self.environment_manager_for_session(&batch_request).await?;
            match environments.active_blocker() {
                Some(ActiveEnvironmentBlocker::NotReady {
                    environment_id,
                    status,
                }) => {
                    return Ok(ToolCallExecution::EnvironmentNotReady {
                        call_id: call.call_id,
                        environment_id: environment_id.clone(),
                        status: *status,
                    });
                }
                Some(blocker @ ActiveEnvironmentBlocker::Unavailable { .. }) => {
                    return failed_result(
                        self.blobs.as_ref(),
                        call.call_id,
                        active_environment_blocker_message(blocker),
                    )
                    .await
                    .map(ToolCallExecution::Completed);
                }
                None => {}
            }
            environments
        } else {
            SessionEnvironmentManager::new(self.blobs.clone())
        };
        if is_job_call {
            let outcome = self
                .invoke_environment_job_call(&batch_request, &call, &environments)
                .await
                .map(ToolCallExecution::Completed);
            environments.close().await;
            return outcome;
        }
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
        let outcome = async {
            let runtime = self
                .runtime_for_domains(
                    links,
                    &environments,
                    batch_request.active_environment_id.as_ref(),
                    batch_request.vfs_working_directory.as_deref(),
                )
                .await?;
            runtime
                .invoke_call(&call)
                .await
                .map(ToolCallExecution::Completed)
        }
        .await;
        environments.close().await;
        outcome
    }
}

impl SessionTools {
    /// Poll the registry (and probe the route) until the environment is
    /// selectable, terminally unusable, or `deadline` passes. `heartbeat` is
    /// invoked on every poll so the hosting activity stays alive.
    pub async fn await_environment_ready(
        &self,
        request: &temporal_workflow::AwaitEnvironmentReadyActivityRequest,
        deadline: tokio::time::Instant,
        heartbeat: impl Fn(),
    ) -> temporal_workflow::AwaitEnvironmentReadyActivityResult {
        use temporal_workflow::AwaitEnvironmentReadyActivityResult as Outcome;
        let Some(resolver) = self.environment_resolver.as_ref() else {
            return Outcome::Failed {
                message: "environment resolver is not configured on this worker".to_owned(),
            };
        };
        let environment_id = match EnvironmentId::try_new(request.environment_id.clone()) {
            Ok(id) => id,
            Err(error) => {
                return Outcome::Failed {
                    message: format!("invalid environment id: {error}"),
                };
            }
        };
        let allowed = request
            .environment_policy
            .as_ref()
            .map(access_policy_from_runtime)
            .unwrap_or_default();
        let mut last_status;
        loop {
            heartbeat();
            let now = i64::try_from(now_unix_ms().unwrap_or_default()).unwrap_or(i64::MAX);
            match resolver.selectable(&environment_id, &allowed, now).await {
                Ok(_) => return Outcome::Ready,
                Err(crate::environment_resolver::EnvironmentResolveError::NotReady {
                    status,
                    ..
                }) => {
                    last_status = format!("{status:?}").to_lowercase();
                }
                Err(
                    crate::environment_resolver::EnvironmentResolveError::EnvironmentUnavailable {
                        status,
                        ..
                    },
                ) => {
                    // Marked ready in the registry but the route probe failed;
                    // keep polling until the deadline, the daemon may still
                    // be coming up.
                    last_status = status;
                }
                Err(crate::environment_resolver::EnvironmentResolveError::Store(
                    environments::EnvironmentRegistryError::Store { message },
                )) => {
                    // Transient store trouble: keep polling.
                    last_status = format!("store error: {message}");
                }
                Err(error) => {
                    return Outcome::Failed {
                        message: error.to_string(),
                    };
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Outcome::TimedOut { last_status };
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep(temporal_workflow::ENVIRONMENT_READY_POLL_INTERVAL.min(remaining))
                .await;
        }
    }
}

/// Model-facing text for a call that cannot use the active environment.
/// A job handle names the session's active environment unless the model
/// says otherwise: `job_submit` and `job_run` only ever start jobs there,
/// so the common read needs just the job id.
fn resolve_job_handle_arg(
    active_environment_id: Option<&EnvironmentId>,
    handle: JobHandleArg,
) -> Result<JobHandle, String> {
    let environment_id = match handle.environment_id {
        Some(environment_id) => EnvironmentId::try_new(environment_id)
            .map_err(|error| format!("invalid job handle environment_id: {error}"))?,
        None => active_environment_id.cloned().ok_or_else(|| {
            "job handle omits environment_id and the session has no active environment".to_owned()
        })?,
    };
    Ok(JobHandle {
        environment_id: environment_id.as_str().to_owned(),
        job_id: handle.job_id,
    })
}

fn active_environment_blocker_message(blocker: &ActiveEnvironmentBlocker) -> String {
    match blocker {
        ActiveEnvironmentBlocker::NotReady {
            environment_id,
            status,
        } => format!(
            "active environment {environment_id} is {} and not reachable yet; retry once it is ready",
            format!("{status:?}").to_lowercase()
        ),
        ActiveEnvironmentBlocker::Unavailable { message } => {
            format!("active environment is unavailable: {message}")
        }
    }
}

/// Cross-call batch rules evaluated from bounded sibling summaries. Only the
/// calls participating in a violation fail; unrelated siblings execute
/// normally.
fn per_call_batch_rule_violation(
    request: &engine::ToolInvocationCallRequest,
) -> Option<&'static str> {
    let call = &request.call;
    let is_env_dependent = |tool_id: Option<&engine::ToolName>| {
        tool_id.is_some_and(|id| BuiltinToolRequirements::for_id(id).active_environment)
    };
    let sibling_selection = request.sibling_calls.iter().any(|sibling| {
        sibling
            .tool_id
            .as_ref()
            .is_some_and(is_environment_selection_tool)
    });
    if call
        .tool_id
        .as_ref()
        .is_some_and(is_environment_selection_tool)
        && (sibling_selection
            || request
                .sibling_calls
                .iter()
                .any(|sibling| is_env_dependent(sibling.tool_id.as_ref())))
    {
        return Some(
            "environment activation/deactivation cannot share a batch with another selection or an environment-dependent tool",
        );
    }
    if is_env_dependent(call.tool_id.as_ref()) && sibling_selection {
        return Some(
            "environment activation/deactivation cannot share a batch with another selection or an environment-dependent tool",
        );
    }
    None
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
        duration_ms: None,
        output_bytes: None,
        truncated: false,
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

fn map_environment_client_error(error: EnvironmentClientError) -> CoreAgentIoError {
    io_error(format!("environment data-plane call failed: {error}"))
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
    use engine::ProviderApiKind;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };
    use tools::concurrency::AWAIT_TOOL_NAME;
    use tools::environment::control::ENVIRONMENT_LIST_TOOL_NAME;

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
    use environment_protocol::shared::{EnvironmentTransport, ProviderTargetId};
    use environments::{
        CreateEnvironment, EnvironmentConnectionSpec, EnvironmentIncarnationId,
        EnvironmentIncarnationRecord, EnvironmentProviderBindingId,
        EnvironmentProviderBindingStatus, EnvironmentProviderBindingStore, EnvironmentProviderId,
        EnvironmentProviderStore, EnvironmentProvisionRequestId, EnvironmentSource,
        EnvironmentStatus, EnvironmentStore, EnvironmentTemplateId,
        InMemoryEnvironmentRegistryStore, ObserveProvisionedEnvironment, PutEnvironmentProvider,
        PutEnvironmentProviderBinding,
    };
    use tools::environment::{
        EnvironmentToolContext,
        process::{
            ContinueProcessRequest, ProcessError, ProcessExecResult, ProcessExecutor,
            ProcessOutput, ProcessRequest, ProcessStatus, StreamOutput,
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
                    .then(|| entry.content.content_ref.clone())
            })
            .expect("visible ref")
    }

    fn test_tool_id(name: &str) -> ToolName {
        ToolName::new(match name {
            "await" => "concurrency.await",
            "cancel" => "concurrency.cancel",
            "detach" => "concurrency.detach",
            "sleep" => "concurrency.sleep",
            "environment_read" => "environment.read",
            "environment_list" => "environment.list",
            "environment_activate" => "environment.activate",
            "environment_deactivate" => "environment.deactivate",
            "read_file" => "env.read_file",
            "run_process" => "env.run_process",
            "job_read" => "env.job_read",
            "vfs_read_file" | "VfsRead" => "vfs.read_file",
            "vfs_materialize" => "vfs.materialize",
            "vfs_capture" => "vfs.capture",
            "web_fetch" => "web.fetch",
            other => other,
        })
    }

    fn test_builtin_runtime() -> engine::BuiltinToolCallRuntime {
        engine::BuiltinToolCallRuntime {
            spec: engine::BuiltinToolSpec {
                settings: serde_json::json!({"presentation":"canonical"}),
            },
            model: engine::ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "test".into(),
                model: "test".into(),
            },
        }
    }

    fn test_environment_policy(
        providers: Option<Vec<String>>,
        keys: Option<Vec<String>>,
    ) -> engine::EnvironmentPolicyRuntime {
        engine::EnvironmentPolicyRuntime {
            tools: Some(engine::EnvironmentToolSurface::Edit),
            commands: true,
            ..engine::EnvironmentPolicyRuntime::new(providers, keys)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ungranted_environment_operations_fail_before_resolving_domains() {
        for tools_grant in [
            None,
            Some(engine::EnvironmentToolSurface::ReadOnly),
            Some(engine::EnvironmentToolSurface::Edit),
        ] {
            for commands in [false, true] {
                let policy = engine::EnvironmentPolicyRuntime {
                    tools: tools_grant,
                    commands,
                    ..engine::EnvironmentPolicyRuntime::new(None, None)
                };
                for (id, allowed) in [
                    ("env.read_file", tools_grant.is_some()),
                    ("env.grep", tools_grant.is_some()),
                    ("env.glob", tools_grant.is_some()),
                    ("env.list_dir", tools_grant.is_some()),
                    (
                        "env.write_file",
                        tools_grant == Some(engine::EnvironmentToolSurface::Edit),
                    ),
                    (
                        "env.edit_file",
                        tools_grant == Some(engine::EnvironmentToolSurface::Edit),
                    ),
                    (
                        "env.apply_patch",
                        tools_grant == Some(engine::EnvironmentToolSurface::Edit),
                    ),
                    (
                        "vfs.materialize",
                        tools_grant == Some(engine::EnvironmentToolSurface::Edit),
                    ),
                    ("vfs.capture", tools_grant.is_some()),
                    ("env.run_process", commands),
                    ("env.continue_process", commands),
                ] {
                    let blobs = Arc::new(InMemoryBlobStore::new());
                    let tools = SessionTools::new(blobs.clone(), Arc::new(TestCatalog::default()));
                    let mut request = per_call_request("read_file", b"{}", &[]);
                    request.call.tool_id = Some(ToolName::new(id));
                    request.environment_policy = Some(policy.clone());
                    assert_eq!(
                        environment_tool_denial(Some(&policy), &request.call).is_none(),
                        allowed,
                        "{id}"
                    );
                    if allowed {
                        continue;
                    }
                    // No active environment, workspace links, or argument blob: refusal
                    // must happen before resolving any of these effectful resources.
                    let call = tools.invoke_call(request.clone()).await.unwrap();
                    let batch = tools
                        .invoke_batch(request.into_batch_request())
                        .await
                        .unwrap()
                        .completed_result()
                        .unwrap();
                    for result in [call, batch.results[0].clone()] {
                        assert_eq!(result.status, ToolCallStatus::Failed);
                        assert!(
                            blobs
                                .read_text(result.error_ref.as_ref().unwrap())
                                .await
                                .unwrap()
                                .contains("not granted")
                        );
                    }
                }
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn denied_environment_call_does_not_block_an_independent_vfs_read() {
        let (blobs, tools, session_id, links) = session_tools_with_readme_link().await;
        let mut request =
            per_call_request("vfs_read_file", br#"{"path":"/workspace/README.md"}"#, &[]);
        request.session_id = session_id;
        request.workspace_links = links;
        request.call.arguments_ref = blobs
            .put_bytes(br#"{"path":"/workspace/README.md"}"#.to_vec())
            .await
            .unwrap();
        let mut denied = request.call.clone();
        denied.call_id = ToolCallId::new("denied");
        denied.tool_id = Some(ToolName::new("env.write_file"));
        let mut batch = request.into_batch_request();
        batch.calls.push(denied);
        let results = tools
            .invoke_batch(batch)
            .await
            .unwrap()
            .completed_result()
            .unwrap()
            .results;
        assert_eq!(results[0].status, ToolCallStatus::Succeeded);
        assert_eq!(results[1].status, ToolCallStatus::Failed);
        assert!(
            blobs
                .read_text(results[1].error_ref.as_ref().unwrap())
                .await
                .unwrap()
                .contains("not granted")
        );
    }

    fn per_call_request(
        tool_name: &str,
        arguments: &[u8],
        siblings: &[(&str, &[u8])],
    ) -> engine::ToolInvocationCallRequest {
        engine::ToolInvocationCallRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: None,
            call: engine::ToolInvocationRequest {
                builtin: Some(test_builtin_runtime()),
                call_id: ToolCallId::new("call_self"),
                tool_id: Some(test_tool_id(tool_name)),
                tool_name: ToolName::new(tool_name),
                arguments_ref: BlobRef::from_bytes(arguments),
                workflow_tool: None,
                promise_control: None,
                remote_mcp: None,
            },
            sibling_calls: siblings
                .iter()
                .enumerate()
                .map(|(index, (name, arguments))| engine::ToolCallSummary {
                    call_id: ToolCallId::new(format!("call_sibling_{index}")),
                    tool_id: Some(test_tool_id(name)),
                    tool_name: ToolName::new(*name),
                    arguments_ref: BlobRef::from_bytes(arguments),
                })
                .collect(),
            execution: engine::ToolExecutionSpec::default(),
        }
    }

    #[test]
    fn per_call_batch_rules_flag_only_participating_calls() {
        for transfer in ["vfs_materialize", "vfs_capture"] {
            assert!(
                per_call_batch_rule_violation(&per_call_request(
                    transfer,
                    b"{}",
                    &[("environment_activate", b"{}")],
                ))
                .is_some()
            );
            assert!(
                per_call_batch_rule_violation(&per_call_request(
                    "environment_activate",
                    b"{}",
                    &[(transfer, b"{}")],
                ))
                .is_some()
            );
        }
        // A selection call with an environment-dependent sibling fails, and
        // an environment-dependent call with a selection sibling fails.
        assert!(
            per_call_batch_rule_violation(&per_call_request(
                "environment_activate",
                b"{}",
                &[("read_file", b"{}")]
            ),)
            .is_some()
        );
        assert!(
            per_call_batch_rule_violation(&per_call_request(
                "read_file",
                b"{}",
                &[("environment_activate", b"{}")]
            ),)
            .is_some()
        );
        // Two selection calls in one batch both fail.
        assert!(
            per_call_batch_rule_violation(&per_call_request(
                "environment_activate",
                b"{}",
                &[("environment_deactivate", b"{}")],
            ),)
            .is_some()
        );
        // An unrelated sibling in the same violating batch is untouched.
        assert!(
            per_call_batch_rule_violation(&per_call_request(
                "web_fetch",
                b"{}",
                &[("environment_activate", b"{}"), ("read_file", b"{}")],
            ),)
            .is_none()
        );
        // A lone selection call is allowed.
        assert!(
            per_call_batch_rule_violation(&per_call_request(
                "environment_activate",
                b"{}",
                &[("web_fetch", b"{}")]
            ),)
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
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
            builtin: None,
            call_id: ToolCallId::new("call-invalid-schema"),
            tool_id: Some(binding.definition.tool.name.clone()),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: invalid_arguments,
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                binding.clone(),
                prior_emission_count,
            )),
            promise_control: None,
            remote_mcp: None,
        }];
        calls.push(engine::ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-name-mismatch"),
            tool_id: Some(ToolName::new("other_tool")),
            tool_name: ToolName::new("other_tool"),
            arguments_ref: valid_arguments.clone(),
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                binding.clone(),
                prior_emission_count,
            )),
            promise_control: None,
            remote_mcp: None,
        });
        calls.push(engine::ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-fingerprint-mismatch"),
            tool_id: Some(binding.definition.tool.name.clone()),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: valid_arguments.clone(),
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                corrupt_binding,
                prior_emission_count,
            )),
            promise_control: None,
            remote_mcp: None,
        });
        calls.push(engine::ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-missing-runtime"),
            tool_id: Some(binding.definition.tool.name.clone()),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: valid_arguments.clone(),
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        });
        calls.extend(
            (0..engine::MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN - prior_emission_count).map(|index| {
                engine::ToolInvocationRequest {
                    builtin: None,
                    call_id: ToolCallId::new(format!("call-{index}")),
                    tool_id: Some(binding.definition.tool.name.clone()),
                    tool_name: binding.definition.tool.name.clone(),
                    arguments_ref: valid_arguments.clone(),
                    workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                        binding.clone(),
                        prior_emission_count,
                    )),
                    promise_control: None,
                    remote_mcp: None,
                }
            }),
        );
        calls.push(engine::ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-over-cap"),
            tool_id: Some(binding.definition.tool.name.clone()),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: valid_arguments,
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(
                binding.clone(),
                prior_emission_count,
            )),
            promise_control: None,
            remote_mcp: None,
        });
        let request = ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id,
            run_id: RunId::new(9),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: None,
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
                metadata: Default::default(),
                session_id: SessionId::new("unrelated-session-created-after-scheduling"),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
                key_source: engine::WorkflowToolCompletionKeySource::ArrayItemField {
                    pointer: "/jobs".to_owned(),
                    field: "job_id".to_owned(),
                },
            },
        )
        .expect("admit environment job binding");
        let arguments_ref = blobs
            .put_bytes(br#"{"jobs":[{"job_id":"build","argv":["make"]}]}"#.to_vec())
            .await
            .expect("put job arguments");
        let call = engine::ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-job-start"),
            tool_id: Some(binding.definition.tool.name.clone()),
            tool_name: binding.definition.tool.name.clone(),
            arguments_ref: arguments_ref.clone(),
            workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(binding.clone(), 0)),
            promise_control: None,
            remote_mcp: None,
        };
        let mut request = ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-job-start"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            active_environment_id: Some(EnvironmentId::new("environment-original")),
            environment_policy: Some(engine::EnvironmentPolicyRuntime::new(
                Some(vec!["provider-b".to_owned(), "provider-a".to_owned()]),
                None,
            )),
            subagents_policy: None,
            workspace_links: Vec::new(),
            calls: vec![call.clone()],
        };
        request
            .environment_policy
            .as_mut()
            .unwrap()
            .working_directory = Some("/project".into());
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
        assert_eq!(context.working_directory.as_deref(), Some("/project"));
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
                vfs_working_directory: None,
                session_id: SessionId::new("session-job-start"),
                run_id: RunId::new(2),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: Some(engine::EnvironmentPolicyRuntime::new(None, None)),
                subagents_policy: None,
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

    /// The `agent_run` system binding as the gateway admits it: a
    /// start-on-call recipe with joined completion.
    async fn agent_run_binding(blobs: &InMemoryBlobStore) -> engine::WorkflowToolBinding {
        let kind = tools::subagents::SubagentToolKind::Run;
        let tool = tools::definitions::register(
            "subagent.run",
            Default::default(),
            engine::ToolParallelism::ParallelSafe,
            Default::default(),
        );
        let recipe = b"test subagent recipe".to_vec();
        let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe);
        let recipe_ref = blobs.put_bytes(recipe).await.expect("put subagent recipe");
        engine::WorkflowToolBinding::admit(
            uuid::Uuid::from_u128(1),
            WorkflowToolDefinition {
                tool_id: WorkflowToolId::new(kind.workflow_tool_id()),
                revision: 1,
                semantic_type: kind.semantic_type().to_owned(),
                tool,
            },
            engine::WorkflowToolTarget::Start {
                start: engine::WorkflowStartRef {
                    recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                    revision: 1,
                    recipe_ref,
                    recipe_fingerprint,
                },
            },
            engine::WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: engine::SUBAGENT_DEADLINE_CEILING_MS,
            },
        )
        .expect("admit agent_run binding")
    }

    fn subagents_policy(
        agents: &[&str],
        limits: engine::SubagentLimits,
    ) -> engine::SubagentsFeature {
        engine::SubagentsFeature {
            agents: agents
                .iter()
                .map(|profile_id| engine::SubagentAgentConfig {
                    profile_id: (*profile_id).to_owned(),
                })
                .collect(),
            limits,
            ..engine::SubagentsFeature::default()
        }
    }

    async fn agent_run_batch(
        blobs: &InMemoryBlobStore,
        binding: &engine::WorkflowToolBinding,
        arguments: &[u8],
        policy: Option<engine::SubagentsFeature>,
    ) -> ToolInvocationBatchRequest {
        let arguments_ref = blobs
            .put_bytes(arguments.to_vec())
            .await
            .expect("put agent arguments");
        ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-parent"),
            run_id: RunId::new(7),
            turn_id: TurnId::new(2),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: policy,
            workspace_links: Vec::new(),
            calls: vec![engine::ToolInvocationRequest {
                builtin: Some(test_builtin_runtime()),
                call_id: ToolCallId::new("call-agent-run"),
                tool_id: Some(binding.definition.tool.name.clone()),
                tool_name: ToolName::new("agent_run"),
                arguments_ref,
                workflow_tool: Some(engine::WorkflowToolCallRuntime::v1(binding.clone(), 0)),
                promise_control: None,
                remote_mcp: None,
            }],
        }
    }

    async fn failure_text(blobs: &InMemoryBlobStore, result: &ToolInvocationResult) -> String {
        assert_eq!(result.status, ToolCallStatus::Failed);
        assert!(result.effects.is_empty(), "a refused call must not emit");
        blobs
            .read_text(result.error_ref.as_ref().expect("error ref"))
            .await
            .expect("read error")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_run_requires_the_subagents_grant() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let binding = agent_run_binding(&blobs).await;
        let tools = SessionTools::new(blobs.clone(), Arc::new(TestCatalog::default()));
        let request = agent_run_batch(
            &blobs,
            &binding,
            br#"{"agent":"reviewer","input":"review PR 1"}"#,
            None,
        )
        .await;

        let outcome = tools
            .invoke_batch(request)
            .await
            .expect("invoke agent_run")
            .completed_result()
            .expect("completed batch");
        let error = failure_text(&blobs, &outcome.results[0]).await;
        assert!(error.contains("requires the subagents grant"), "{error}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_run_rejects_agents_outside_the_catalog_and_invalid_briefs() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let binding = agent_run_binding(&blobs).await;
        let tools = SessionTools::new(blobs.clone(), Arc::new(TestCatalog::default()));
        let policy = subagents_policy(&["reviewer", "planner"], engine::SubagentLimits::default());

        let unlisted = tools
            .invoke_batch(
                agent_run_batch(
                    &blobs,
                    &binding,
                    br#"{"agent":"intruder","input":"review PR 1"}"#,
                    Some(policy.clone()),
                )
                .await,
            )
            .await
            .expect("invoke unlisted agent")
            .completed_result()
            .expect("completed batch");
        let error = failure_text(&blobs, &unlisted.results[0]).await;
        assert!(
            error.contains("intruder is not in this session's sub-agent catalog")
                && error.contains("allowed: reviewer, planner"),
            "{error}"
        );

        let blank = tools
            .invoke_batch(
                agent_run_batch(
                    &blobs,
                    &binding,
                    br#"{"agent":"reviewer","input":"   "}"#,
                    Some(policy),
                )
                .await,
            )
            .await
            .expect("invoke blank brief")
            .completed_result()
            .expect("completed batch");
        let error = failure_text(&blobs, &blank.results[0]).await;
        assert!(error.contains("input must be a non-empty brief"), "{error}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_run_pins_the_grant_and_parent_identity_in_the_execution_context() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let binding = agent_run_binding(&blobs).await;
        let tools = SessionTools::new(blobs.clone(), Arc::new(TestCatalog::default()));
        let limits = engine::SubagentLimits {
            max_depth: 1,
            max_descendants: 3,
            max_concurrent: 2,
            deadline_ms: 45_000,
        };
        let request = agent_run_batch(
            &blobs,
            &binding,
            br#"{"agent":"reviewer","input":"review PR 1","label":"reviewer: PR 1"}"#,
            Some(subagents_policy(&["reviewer"], limits)),
        )
        .await;

        let first = tools
            .invoke_batch(request.clone())
            .await
            .expect("invoke agent_run")
            .completed_result()
            .expect("completed batch");
        assert_eq!(first.results[0].status, ToolCallStatus::Succeeded);
        let effect = &first.results[0].effects[0];
        assert_eq!(
            effect.data.get("arguments_ref").map(String::as_str),
            Some(request.calls[0].arguments_ref.as_str()),
            "the model arguments stay in CAS untouched"
        );
        let context_ref = BlobRef::parse(
            effect
                .data
                .get("execution_context_ref")
                .expect("execution context ref")
                .clone(),
        )
        .expect("valid execution context ref");
        let context: SubagentExecutionContextV1 = serde_json::from_slice(
            &blobs
                .read_bytes(&context_ref)
                .await
                .expect("read execution context"),
        )
        .expect("decode execution context");
        assert_eq!(
            context,
            SubagentExecutionContextV1::new(
                "session-parent".to_owned(),
                7,
                "reviewer".to_owned(),
                limits
            )
        );
        assert_eq!(context.version, SubagentExecutionContextV1::VERSION);

        // Admission is idempotent per call identity; only the joined
        // completion's wall-clock deadline moves between attempts.
        let mut retried = tools
            .invoke_batch(request)
            .await
            .expect("retry agent_run")
            .completed_result()
            .expect("completed retry");
        let mut first = first;
        for result in [&mut first, &mut retried] {
            for effect in &mut result.results[0].effects {
                assert!(effect.data.remove("completion_deadline_ms").is_some());
            }
        }
        assert_eq!(retried, first);
    }

    #[derive(Default)]
    struct TestCatalog {
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
    }

    #[derive(Default)]
    struct RecordingProcessExecutor {
        requests: Mutex<Vec<ProcessRequest>>,
    }

    #[async_trait]
    impl ProcessExecutor for RecordingProcessExecutor {
        async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            self.requests.lock().expect("process lock").push(request);
            Ok(ProcessOutput {
                status: ProcessStatus::Succeeded,
                handle: None,
                pid: Some(1),
                exit_code: Some(0),
                failure: None,
                stdout: StreamOutput {
                    bytes: b"process ok".to_vec(),
                    omitted_at: None,
                },
                stderr: StreamOutput::default(),
                omitted_bytes: 0,
                leftover_processes: Vec::new(),
            })
        }

        async fn continue_process(
            &self,
            _request: ContinueProcessRequest,
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
            None,
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
        let target_id = ProviderTargetId::new("test");
        let resource = environments::EnvironmentRecord {
            environment_id: engine::EnvironmentId::new("test"),
            request_id: EnvironmentProvisionRequestId::new("request-test"),
            source: EnvironmentSource::Provisioned {
                provider_id: EnvironmentProviderId::new("test-provider"),
                binding_id: EnvironmentProviderBindingId::new("test-binding"),
            },
            display_name: None,
            status: EnvironmentStatus::Offline,
            desired_power: environments::PowerState::Running,
            idle_policy: None,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: EnvironmentIncarnationId::new("incarnation-test"),
                provision_request_id: Some(EnvironmentProvisionRequestId::new("request-test")),
                provider_target_id: Some(target_id.clone()),
                template_id: Some(EnvironmentTemplateId::new("test-template")),
                adoption_source_target: None,
                power_states: Vec::new(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            origin_session: None,
            metadata: BTreeMap::new(),
            last_seen_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let fs_context = tools::fs::FsToolContext::new(
            Arc::new(tools::fs::InMemoryFileSystem::full_access()),
            blobs.clone(),
        );
        let tool_context = EnvironmentToolContext::new(Some(process), blobs)
            .with_process_cwd(FsPath::new("/workspace").expect("process cwd"))
            .with_filesystem(fs_context);
        RuntimeEnvironment::from_resource(resource, tool_context)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod transfers {
        use super::*;
        use environment_protocol::data::transfer_session::{TransferRequest, TransferResponse};

        struct LocalTransfer(environment_daemon::filesystem::LocalFileSystem);

        #[async_trait]
        impl tools::transfer::EnvironmentTransfer for LocalTransfer {
            async fn request(
                &self,
                request: TransferRequest,
            ) -> tools::ToolResult<TransferResponse> {
                self.0
                    .transfer(request)
                    .await
                    .map_err(|error| tools::ToolError::InvalidRequest {
                        message: error.message,
                    })
            }
        }

        async fn dispatch(
            tools: &SessionTools,
            request: engine::ToolInvocationCallRequest,
            batch: bool,
        ) -> ToolInvocationResult {
            if batch {
                tools
                    .invoke_batch(request.into_batch_request())
                    .await
                    .unwrap()
                    .completed_result()
                    .unwrap()
                    .results
                    .remove(0)
            } else {
                tools.invoke_call(request).await.unwrap()
            }
        }

        async fn succeeded(
            blobs: &InMemoryBlobStore,
            result: &ToolInvocationResult,
        ) -> serde_json::Value {
            if result.status != ToolCallStatus::Succeeded {
                panic!(
                    "{}",
                    blobs
                        .read_text(result.error_ref.as_ref().unwrap())
                        .await
                        .unwrap()
                );
            }
            serde_json::from_slice(
                &blobs
                    .read_bytes(result.output_ref.as_ref().unwrap())
                    .await
                    .unwrap(),
            )
            .unwrap()
        }

        #[tokio::test(flavor = "current_thread")]
        async fn hosted_transfer_dispatch_loads_both_domains_and_publishes_capture() {
            for batch in [false, true] {
                for api_kind in [
                    ProviderApiKind::OpenAiResponses,
                    ProviderApiKind::AnthropicMessages,
                    ProviderApiKind::OpenAiCompletions,
                ] {
                    let (blobs, tools, session_id, workspace_links) =
                        session_tools_with_readme_link().await;
                    let root = tempfile::tempdir().unwrap();
                    let fs = environment_daemon::filesystem::LocalFileSystem::new(
                        root.path().into(),
                        root.path().into(),
                        true,
                    );
                    let environment = test_environment(
                        blobs.clone(),
                        Arc::new(RecordingProcessExecutor::default()),
                    );
                    let mut context = environment.tool_context().clone();
                    context.process_cwd = Some(FsPath::new(root.path().to_string_lossy()).unwrap());
                    context.transfer = Some(Arc::new(LocalTransfer(fs)));
                    let environment =
                        RuntimeEnvironment::from_resource(environment.resource().clone(), context);
                    let tools = tools
                        .with_blob_graph(blobs.clone())
                        .with_environment(environment);
                    let args = serde_json::to_vec(&serde_json::json!({
                        "source_vfs_path": "/workspace/README.md",
                        "destination_environment_path": "./created/nested/README.md",
                    }))
                    .unwrap();
                    let mut request = per_call_request("vfs_materialize", &args, &[]);
                    request.session_id = session_id;
                    request.workspace_links = workspace_links.clone();
                    request.active_environment_id = Some(EnvironmentId::new("test"));
                    request.environment_policy = Some(test_environment_policy(None, None));
                    request.call.arguments_ref = blobs.put_bytes(args).await.unwrap();
                    let builtin = request.call.builtin.as_mut().unwrap();
                    builtin.model.api_kind = api_kind;
                    builtin.spec.settings = serde_json::json!({"presentation":"provider_default"});
                    let result = dispatch(&tools, request.clone(), batch).await;
                    succeeded(&blobs, &result).await;
                    assert_eq!(
                        std::fs::read(root.path().join("created/nested/README.md")).unwrap(),
                        b"hello\n"
                    );

                    // A completed retry through the hosted path must keep its operation
                    // identity and preserve edits made after publication.
                    std::fs::write(
                        root.path().join("created/nested/README.md"),
                        b"environment edit",
                    )
                    .unwrap();
                    succeeded(&blobs, &dispatch(&tools, request.clone(), batch).await).await;
                    assert_eq!(
                        std::fs::read(root.path().join("created/nested/README.md")).unwrap(),
                        b"environment edit"
                    );

                    let mut capture = request.clone();
                    capture.call.call_id = ToolCallId::new("capture");
                    capture.call.tool_id = Some(test_tool_id("vfs_capture"));
                    capture.call.tool_name = ToolName::new("vfs_capture");
                    capture.call.arguments_ref = blobs
                        .put_bytes(
                            serde_json::to_vec(&serde_json::json!({
                                "source_environment_path":"./created/nested/README.md",
                                "destination_vfs_path":"/workspace/results/nested/captured.txt",
                            }))
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    let result = dispatch(&tools, capture.clone(), batch).await;
                    let output = succeeded(&blobs, &result).await;
                    assert_eq!(output["published"], true);
                    let workspace = tools
                        .workspace_store
                        .read_workspace(&VfsWorkspaceId::new("workspace_1"))
                        .await
                        .unwrap();
                    let manifest =
                        vfs::read_snapshot_manifest(blobs.as_ref(), &workspace.head_snapshot_ref)
                            .await
                            .unwrap();
                    assert_eq!(
                        vfs::read_snapshot_file(
                            blobs.as_ref(),
                            &manifest,
                            &vfs::VfsPath::parse("/results/nested/captured.txt").unwrap()
                        )
                        .await
                        .unwrap(),
                        b"environment edit"
                    );
                    assert_eq!(
                        vfs::read_snapshot_file(
                            blobs.as_ref(),
                            &manifest,
                            &vfs::VfsPath::parse("/README.md").unwrap()
                        )
                        .await
                        .unwrap(),
                        b"hello\n"
                    );
                    let output_ref = result.output_ref.unwrap();
                    let snapshot_ref =
                        BlobRef::parse(output["snapshot_ref"].as_str().unwrap()).unwrap();
                    assert!(
                        blobs
                            .edges()
                            .contains(&BlobEdge::contains(output_ref, snapshot_ref))
                    );
                    assert!(
                        !result.effects.is_empty(),
                        "workspace effects must be drained from the VFS context"
                    );

                    // Link permissions are enforced even if a call has an admitted
                    // editing-tool identity.
                    capture.call.call_id = ToolCallId::new("capture-readonly");
                    capture.workspace_links[0].access = WorkspaceLinkAccess::ReadOnly;
                    assert_eq!(
                        dispatch(&tools, capture, batch).await.status,
                        ToolCallStatus::Failed
                    );
                    request.workspace_links.clear();
                    assert_eq!(
                        dispatch(&tools, request.clone(), batch).await.status,
                        ToolCallStatus::Failed
                    );
                    request.workspace_links = workspace_links;
                    request.active_environment_id = None;
                    assert_eq!(
                        dispatch(&tools, request, batch).await.status,
                        ToolCallStatus::Failed
                    );
                }
            }
        }
    }

    async fn register_test_environment_provider(
        store: &InMemoryEnvironmentRegistryStore,
        provider_id: &str,
    ) {
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: EnvironmentProviderId::new(provider_id),
                display_name: None,
                controller_connection: EnvironmentConnectionSpec::new(
                    "http://controller.test",
                    EnvironmentTransport::Http,
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
        let target_id = ProviderTargetId::new(format!("target-{environment_id}"));
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
                origin_session: None,
                idle_policy: None,
                created_at_ms: observed_at_ms.saturating_sub(1),
            })
            .await
            .expect("create environment");
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id,
                provider_target_id: target_id,
                status: EnvironmentStatus::Offline,
                power_states: Vec::new(),
                observed_at_ms,
            })
            .await
            .expect("observe environment");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn environment_read_status_message_only_describes_supported_wake_on_use() {
        use environments::PowerState;

        for (status, power_states, expected_status, expected_message) in [
            (
                EnvironmentStatus::Paused,
                vec![PowerState::Running],
                "paused",
                true,
            ),
            (
                EnvironmentStatus::Suspended,
                vec![PowerState::Running],
                "suspended",
                true,
            ),
            (
                EnvironmentStatus::Offline,
                vec![PowerState::Running],
                "offline",
                true,
            ),
            (EnvironmentStatus::Paused, Vec::new(), "paused", false),
            (EnvironmentStatus::Suspended, Vec::new(), "suspended", false),
            (EnvironmentStatus::Offline, Vec::new(), "offline", false),
            (
                EnvironmentStatus::Ready,
                vec![PowerState::Running],
                "ready",
                false,
            ),
            (
                EnvironmentStatus::Failed,
                vec![PowerState::Running],
                "failed",
                false,
            ),
        ] {
            let blobs = Arc::new(InMemoryBlobStore::new());
            let registry = Arc::new(InMemoryEnvironmentRegistryStore::new());
            register_test_environment_provider(registry.as_ref(), "allowed").await;
            let environment_id = EnvironmentId::new("environment-allowed-1");
            observe_test_environment(registry.as_ref(), environment_id.as_str(), "allowed", 10)
                .await;
            registry
                .observe_provisioned_environment(ObserveProvisionedEnvironment {
                    environment_id: environment_id.clone(),
                    provider_target_id: ProviderTargetId::new("target-environment-allowed-1"),
                    status,
                    power_states,
                    observed_at_ms: 11,
                })
                .await
                .expect("observe status and power support");
            let resolver =
                crate::environment_resolver::EnvironmentResolver::new(registry.clone(), registry);
            let tools = SessionTools::new(blobs.clone(), Arc::new(TestCatalog::default()))
                .with_environment_resolver(resolver);
            blobs.put_bytes(b"{}".to_vec()).await.expect("arguments");

            for tool_name in ["environment_read", "environment_list"] {
                let mut request = per_call_request(tool_name, b"{}", &[]);
                request.active_environment_id = Some(environment_id.clone());
                request.environment_policy = Some(test_environment_policy(
                    Some(vec!["allowed".to_owned()]),
                    None,
                ));
                let result = tools.invoke_call(request).await.expect("invoke tool");
                assert_eq!(result.status, ToolCallStatus::Succeeded);
                let output: serde_json::Value = serde_json::from_str(
                    &blobs
                        .read_text(&visible_tool_result_ref(&result))
                        .await
                        .expect("model-visible output"),
                )
                .expect("decode output");
                let view = if tool_name == "environment_read" {
                    &output
                } else {
                    &output["environments"][0]
                };
                assert_eq!(view["status"], expected_status);
                if tool_name == "environment_read" && expected_message {
                    assert_eq!(
                        view["status_message"],
                        format!(
                            "Environment is {expected_status}. Tools that use this environment will automatically wake it and wait until it is ready. You can proceed normally."
                        ),
                    );
                } else {
                    assert!(view.get("status_message").is_none(), "{tool_name}: {view}");
                }
            }
        }
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
            vfs_working_directory: None,
            session_id: SessionId::new("session-per-call-list"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: Some(EnvironmentId::new("environment-allowed-1")),
            environment_policy: Some(test_environment_policy(
                Some(vec!["allowed".to_owned()]),
                None,
            )),
            subagents_policy: None,
            call: engine::ToolInvocationRequest {
                builtin: Some(test_builtin_runtime()),
                call_id: ToolCallId::new("call-environment-list"),
                tool_id: Some(test_tool_id(ENVIRONMENT_LIST_TOOL_NAME)),
                tool_name: ToolName::new(ENVIRONMENT_LIST_TOOL_NAME),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
                remote_mcp: None,
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
    async fn per_call_environment_tool_reports_not_ready_instead_of_running() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let registry = Arc::new(InMemoryEnvironmentRegistryStore::new());
        register_test_environment_provider(registry.as_ref(), "allowed").await;
        // Created but never observed: the environment is still provisioning.
        registry
            .create_environment(CreateEnvironment {
                request_id: EnvironmentProvisionRequestId::new("request-pending"),
                environment_id: EnvironmentId::new("environment-pending"),
                incarnation_id: EnvironmentIncarnationId::new("incarnation-pending"),
                binding_id: EnvironmentProviderBindingId::new("binding-allowed"),
                template_id: EnvironmentTemplateId::new("test-template"),
                display_name: None,
                metadata: BTreeMap::new(),
                origin_session: None,
                idle_policy: None,
                created_at_ms: 10,
            })
            .await
            .expect("create environment");
        let resolver = crate::environment_resolver::EnvironmentResolver::new(
            registry.clone(),
            registry.clone(),
        );
        let tools = SessionTools::new(blobs.clone(), catalog).with_environment_resolver(resolver);
        let arguments_ref = blobs
            .put_bytes(br#"{"path":"README.md"}"#.to_vec())
            .await
            .expect("read_file arguments");
        let request = engine::ToolInvocationCallRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-per-call-not-ready"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: Some(EnvironmentId::new("environment-pending")),
            environment_policy: Some(test_environment_policy(None, None)),
            subagents_policy: None,
            call: engine::ToolInvocationRequest {
                builtin: Some(test_builtin_runtime()),
                call_id: ToolCallId::new("call-read-file"),
                tool_id: Some(test_tool_id("read_file")),
                tool_name: ToolName::new("read_file"),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
                remote_mcp: None,
            },
            sibling_calls: Vec::new(),
            execution: engine::ToolExecutionSpec::default(),
        };

        // Hosted per-call path: the call does not run and reports not-ready.
        for name in ["vfs_materialize", "vfs_capture"] {
            let mut transfer = request.clone();
            transfer.call.tool_id = Some(test_tool_id(name));
            transfer.call.tool_name = ToolName::new(name);
            assert!(matches!(
                tools.invoke_call_execution(transfer).await.unwrap(),
                ToolCallExecution::EnvironmentNotReady { .. }
            ));
        }
        let execution = tools
            .invoke_call_execution(request.clone())
            .await
            .expect("invoke call execution");
        assert!(matches!(
            execution,
            ToolCallExecution::EnvironmentNotReady {
                ref environment_id,
                status: EnvironmentStatus::Provisioning,
                ..
            } if environment_id == "environment-pending"
        ));

        // The generic trait path degrades to an ordinary failed result.
        let result = tools.invoke_call(request).await.expect("invoke call");
        assert_eq!(result.status, engine::ToolCallStatus::Failed);

        // Once observed ready, resolution no longer blocks (the route probe
        // itself is exercised by live tests).
        registry
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id: EnvironmentId::new("environment-pending"),
                provider_target_id: ProviderTargetId::new("target-pending"),
                status: EnvironmentStatus::Ready,
                power_states: Vec::new(),
                observed_at_ms: 20,
            })
            .await
            .expect("observe ready");
        let outcome = tools
            .await_environment_ready(
                &temporal_workflow::AwaitEnvironmentReadyActivityRequest {
                    session_id: SessionId::new("session-per-call-not-ready"),
                    environment_id: "environment-pending".to_owned(),
                    environment_policy: None,
                },
                tokio::time::Instant::now() + std::time::Duration::from_millis(50),
                || {},
            )
            .await;
        // No gateway on this runtime, so the probe fails and the bounded wait
        // times out rather than returning a false Ready.
        assert!(matches!(
            outcome,
            temporal_workflow::AwaitEnvironmentReadyActivityResult::TimedOut { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_environment_ready_fails_fast_on_a_failed_environment() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let registry = Arc::new(InMemoryEnvironmentRegistryStore::new());
        register_test_environment_provider(registry.as_ref(), "allowed").await;
        observe_test_environment(registry.as_ref(), "environment-failed", "allowed", 10).await;
        registry
            .fail_environment_lifecycle(environments::FailEnvironmentLifecycle {
                environment_id: EnvironmentId::new("environment-failed"),
                message: "no capacity".to_owned(),
                observed_at_ms: 11,
            })
            .await
            .expect("fail environment");
        let resolver = crate::environment_resolver::EnvironmentResolver::new(
            registry.clone(),
            registry.clone(),
        );
        let tools = SessionTools::new(blobs, Arc::new(TestCatalog::default()))
            .with_environment_resolver(resolver);
        let outcome = tools
            .await_environment_ready(
                &temporal_workflow::AwaitEnvironmentReadyActivityRequest {
                    session_id: SessionId::new("session-failed"),
                    environment_id: "environment-failed".to_owned(),
                    environment_policy: None,
                },
                tokio::time::Instant::now() + std::time::Duration::from_secs(30),
                || {},
            )
            .await;
        assert!(matches!(
            outcome,
            temporal_workflow::AwaitEnvironmentReadyActivityResult::Failed { message }
                if message.contains("no capacity")
        ));
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
            vfs_working_directory: None,
            session_id: SessionId::new("session-per-call-await"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: None,
            call: engine::ToolInvocationRequest {
                builtin: Some(test_builtin_runtime()),
                call_id: ToolCallId::new("call-await"),
                tool_id: Some(test_tool_id(AWAIT_TOOL_NAME)),
                tool_name: ToolName::new(AWAIT_TOOL_NAME),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
                remote_mcp: None,
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
            vfs_working_directory: None,
            session_id: SessionId::new("session-environment-list"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            active_environment_id: Some(EnvironmentId::new("environment-allowed-1")),
            environment_policy: Some(test_environment_policy(
                Some(vec!["allowed".to_owned()]),
                None,
            )),
            subagents_policy: None,
            workspace_links: Vec::new(),
            calls: vec![engine::ToolInvocationRequest {
                builtin: Some(test_builtin_runtime()),
                call_id: ToolCallId::new("call-environment-list"),
                tool_id: Some(test_tool_id(ENVIRONMENT_LIST_TOOL_NAME)),
                tool_name: ToolName::new(ENVIRONMENT_LIST_TOOL_NAME),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
                remote_mcp: None,
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
    async fn vfs_relative_paths_use_only_the_configured_directory() {
        for (cwd, succeeds) in [(None, false), (Some("/workspace"), true)] {
            let (blobs, tools, session_id, links) = session_tools_with_readme_link().await;
            let mut request = per_call_request("vfs_read_file", br#"{"path":"README.md"}"#, &[]);
            request.session_id = session_id;
            request.workspace_links = links;
            request.vfs_working_directory = cwd.map(String::from);
            request.call.arguments_ref = blobs
                .put_bytes(br#"{"path":"README.md"}"#.to_vec())
                .await
                .unwrap();
            let result = tools.invoke_call(request).await.unwrap();
            assert_eq!(result.status == ToolCallStatus::Succeeded, succeeds);
            if succeeds {
                assert!(
                    blobs
                        .read_text(result.output_ref.as_ref().unwrap())
                        .await
                        .unwrap()
                        .contains("hello")
                );
            }
        }
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
                vfs_working_directory: Some("/workspace".into()),
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links,
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(test_builtin_runtime()),
                    call_id: ToolCallId::new("call_1"),
                    tool_id: Some(test_tool_id("vfs_read_file")),
                    tool_name: ToolName::new("vfs_read_file"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
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
                vfs_working_directory: Some("/workspace".into()),
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links,
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(engine::BuiltinToolCallRuntime {
                        spec: engine::BuiltinToolSpec::default(),
                        model: engine::ModelSelection {
                            api_kind: ProviderApiKind::AnthropicMessages,
                            provider_id: "anthropic".into(),
                            model: "claude-test".into(),
                        },
                    }),
                    call_id: ToolCallId::new("call_1"),
                    tool_id: Some(test_tool_id("VfsRead")),
                    tool_name: ToolName::new("VfsRead"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
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
                vfs_working_directory: Some("/workspace".into()),
                session_id,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: Some(EnvironmentId::new("test")),
                environment_policy: Some(test_environment_policy(None, None)),
                subagents_policy: None,
                workspace_links,
                calls: vec![
                    engine::ToolInvocationRequest {
                        builtin: Some(test_builtin_runtime()),
                        call_id: ToolCallId::new("call_read"),
                        tool_id: Some(test_tool_id("vfs_read_file")),
                        tool_name: ToolName::new("vfs_read_file"),
                        arguments_ref: read_args,
                        workflow_tool: None,
                        promise_control: None,
                        remote_mcp: None,
                    },
                    engine::ToolInvocationRequest {
                        builtin: Some(test_builtin_runtime()),
                        call_id: ToolCallId::new("call_process"),
                        // The canonical surface takes argv; `exec_command` is
                        // the Codex-like shape and takes a shell string.
                        tool_id: Some(test_tool_id("run_process")),
                        tool_name: ToolName::new("run_process"),
                        arguments_ref: process_args,
                        workflow_tool: None,
                        promise_control: None,
                        remote_mcp: None,
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
    async fn await_in_mixed_batch_defers_with_completed_non_await_results() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: parent.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
                            promise_id: engine::PromiseId::new("promise_1"),
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
        let tools = SessionTools::new(blobs.clone(), catalog.clone());
        let wait_args = blobs
            .put_bytes(br#"{"promises":["promise_1"]}"#.to_vec())
            .await
            .expect("await args");
        let read_args = blobs
            .put_bytes(br#"{"path":"README.md"}"#.to_vec())
            .await
            .expect("read args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![
                    engine::ToolInvocationRequest {
                        builtin: Some(test_builtin_runtime()),
                        call_id: ToolCallId::new("call_wait"),
                        tool_id: Some(test_tool_id(::tools::concurrency::AWAIT_TOOL_NAME)),
                        tool_name: ToolName::new(::tools::concurrency::AWAIT_TOOL_NAME),
                        arguments_ref: wait_args,
                        workflow_tool: None,
                        promise_control: None,
                        remote_mcp: None,
                    },
                    engine::ToolInvocationRequest {
                        builtin: Some(test_builtin_runtime()),
                        call_id: ToolCallId::new("call_read"),
                        tool_id: Some(test_tool_id("read_file")),
                        tool_name: ToolName::new("read_file"),
                        arguments_ref: read_args,
                        workflow_tool: None,
                        promise_control: None,
                        remote_mcp: None,
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
        assert_eq!(spec.promise_ids, vec![engine::PromiseId::new("promise_1")]);
        assert_eq!(completed_results.len(), 1);
        assert_eq!(completed_results[0].call_id, ToolCallId::new("call_read"));
        assert_eq!(completed_results[0].status, ToolCallStatus::Failed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_defers_without_session_store() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = SessionId::new("parent_no_fleet_await");
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: parent.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
                                    promise_id: engine::PromiseId::new("promise_1"),
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
            .put_bytes(br#"{"promises":["promise_1"]}"#.to_vec())
            .await
            .expect("await args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(test_builtin_runtime()),
                    call_id: ToolCallId::new("call_wait"),
                    tool_id: Some(test_tool_id(::tools::concurrency::AWAIT_TOOL_NAME)),
                    tool_name: ToolName::new(::tools::concurrency::AWAIT_TOOL_NAME),
                    arguments_ref: wait_args,
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
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
        assert_eq!(spec.promise_ids, vec![engine::PromiseId::new("promise_1")]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_uses_supplied_runtime_without_session_store() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let parent = SessionId::new("parent_no_fleet_cancel");
        let tools = SessionTools::new(blobs.clone(), catalog.clone());
        let cancel_args = blobs
            .put_bytes(br#"{"promises":["promise_1"]}"#.to_vec())
            .await
            .expect("cancel args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: parent,
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(test_builtin_runtime()),
                    call_id: ToolCallId::new("call_cancel"),
                    tool_id: Some(test_tool_id(::tools::concurrency::CANCEL_TOOL_NAME)),
                    tool_name: ToolName::new(::tools::concurrency::CANCEL_TOOL_NAME),
                    arguments_ref: cancel_args,
                    workflow_tool: None,
                    promise_control: Some(engine::PromiseControlCallRuntime::v1(vec![
                        engine::PromiseControlRuntime {
                            promise_id: engine::PromiseId::new("promise_1"),
                            state: engine::PromiseControlStateRuntime::Known {
                                ownership: engine::PromiseOwnership::Model,
                                scope: engine::PromiseScope::Session,
                                promise_status: engine::PromiseStatus::Pending,
                            },
                        },
                    ])),
                    remote_mcp: None,
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
            .put_bytes(br#"{"promises":["promise_1"]}"#.to_vec())
            .await
            .expect("detach args");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: parent,
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(test_builtin_runtime()),
                    call_id: ToolCallId::new("call_detach"),
                    tool_id: Some(test_tool_id(::tools::concurrency::DETACH_TOOL_NAME)),
                    tool_name: ToolName::new(::tools::concurrency::DETACH_TOOL_NAME),
                    arguments_ref: detach_args,
                    workflow_tool: None,
                    promise_control: Some(engine::PromiseControlCallRuntime::v1(vec![
                        engine::PromiseControlRuntime {
                            promise_id: engine::PromiseId::new("promise_1"),
                            state: engine::PromiseControlStateRuntime::Known {
                                ownership: engine::PromiseOwnership::Model,
                                scope: engine::PromiseScope::Run {
                                    run_id: RunId::new(1),
                                },
                                promise_status: engine::PromiseStatus::Pending,
                            },
                        },
                    ])),
                    remote_mcp: None,
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
    async fn sleep_emits_timer_promise_effects_numbered_from_the_batch_base() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog);
        let sleep_args = blobs
            .put_bytes(br#"{"ms":50}"#.to_vec())
            .await
            .expect("sleep args");
        let sleep_call = |id: &str| engine::ToolInvocationRequest {
            builtin: Some(test_builtin_runtime()),
            call_id: ToolCallId::new(id),
            tool_id: Some(test_tool_id(::tools::concurrency::SLEEP_TOOL_NAME)),
            tool_name: ToolName::new(::tools::concurrency::SLEEP_TOOL_NAME),
            arguments_ref: sleep_args.clone(),
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: SessionId::new("session_sleep"),
                run_id: RunId::new(9),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 5,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![sleep_call("call_sleep_a"), sleep_call("call_sleep_b")],
            })
            .await
            .expect("invoke")
            .completed_result()
            .expect("completed");

        // Two promise-creating calls in one batch draw disjoint ids from the
        // engine's base, and the model-visible text names the same handle.
        let mut minted = Vec::new();
        for (index, call_result) in result.results.iter().enumerate() {
            assert_eq!(call_result.status, ToolCallStatus::Succeeded);
            assert_eq!(call_result.effects.len(), 1);
            let effect = &call_result.effects[0];
            assert_eq!(effect.kind, engine::PROMISE_CREATE_EFFECT_KIND);
            assert_eq!(effect.data.get("source"), Some(&"timer".to_owned()));
            assert!(effect.data.contains_key("fire_at_ms"));
            let promise_id = effect.data.get("promise_id").expect("promise id");
            assert_eq!(promise_id, &format!("promise_{}", 5 + index));
            let visible = blobs
                .read_text(call_result.output_ref.as_ref().expect("output"))
                .await
                .expect("output text");
            assert!(visible.contains(promise_id.as_str()), "{visible}");
            minted.push(promise_id.clone());
        }
        assert_eq!(minted, vec!["promise_5", "promise_6"]);
    }

    #[test]
    fn job_handles_default_to_the_active_environment() {
        let active = EnvironmentId::new("environment_active");
        let resolved = resolve_job_handle_arg(
            Some(&active),
            JobHandleArg {
                environment_id: None,
                job_id: environment_protocol::shared::JobId::new("build"),
            },
        )
        .expect("defaults to the active environment");
        assert_eq!(resolved.environment_id, "environment_active");
        assert_eq!(resolved.job_id.as_str(), "build");

        let explicit = resolve_job_handle_arg(
            Some(&active),
            JobHandleArg {
                environment_id: Some("environment_other".to_owned()),
                job_id: environment_protocol::shared::JobId::new("build"),
            },
        )
        .expect("explicit environment wins");
        assert_eq!(explicit.environment_id, "environment_other");

        assert!(
            resolve_job_handle_arg(
                None,
                JobHandleArg {
                    environment_id: None,
                    job_id: environment_protocol::shared::JobId::new("build"),
                },
            )
            .is_err(),
            "no active environment and no explicit id is a tool error"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_tools_fail_vfs_tool_without_workspace_links() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let catalog = Arc::new(TestCatalog::default());
        let tools = SessionTools::new(blobs.clone(), catalog);
        let arguments_ref = BlobRef::from_bytes(b"{}");

        let result = tools
            .invoke_batch(ToolInvocationBatchRequest {
                vfs_working_directory: None,
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(test_builtin_runtime()),
                    call_id: ToolCallId::new("call_1"),
                    tool_id: Some(test_tool_id("vfs_read_file")),
                    tool_name: ToolName::new("vfs_read_file"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
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
                vfs_working_directory: None,
                session_id: SessionId::new("session_1"),
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(1),
                promise_id_base: 1,
                active_environment_id: None,
                environment_policy: None,
                subagents_policy: None,
                workspace_links: Vec::new(),
                calls: vec![engine::ToolInvocationRequest {
                    builtin: Some(test_builtin_runtime()),
                    call_id: ToolCallId::new("call_1"),
                    tool_id: Some(test_tool_id("web_fetch")),
                    tool_name: ToolName::new("web_fetch"),
                    arguments_ref,
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
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
