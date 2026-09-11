use std::sync::Arc;

use engine::{
    BlobRef, ContextCompactionResult, LlmGenerationResult, PromiseSourceCheckResult,
    ToolBatchOutcome,
};
use store_pg::PgStore;
use temporalio_common::error::ApplicationFailure;
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::universe::{UniverseError, UniverseRuntime};
use crate::worker::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_AWAIT_ENVIRONMENT_READY,
    ACTIVITY_CANCEL_WORKFLOW_TOOL_EXECUTION, ACTIVITY_CHECK_WORKFLOW_TOOL_EXECUTION,
    ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION, ACTIVITY_ENVIRONMENT_JOB_CANCEL,
    ACTIVITY_ENVIRONMENT_JOB_POLL, ACTIVITY_ENVIRONMENT_JOB_PREPARE_WORKFLOW_TOOL,
    ACTIVITY_ENVIRONMENT_JOB_START, ACTIVITY_LLM_GENERATE, ACTIVITY_MATERIALIZE_AWAIT_RESULT,
    ACTIVITY_PREPROCESS_RUN_INPUT, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB,
    ACTIVITY_RUNTIME_PROJECTION_REFRESH, ACTIVITY_START_WORKFLOW_TOOL_EXECUTION,
    ACTIVITY_SUBAGENT_CLOSE, ACTIVITY_SUBAGENT_PREPARE, ACTIVITY_SUBAGENT_RESOLVE,
    ACTIVITY_TOOL_INVOKE_BATCH, ACTIVITY_TOOL_INVOKE_CALL, ACTIVITY_TOOL_PREPARE_PROMISE_CONTROLS,
    ACTIVITY_VALIDATE_WORKFLOW_TOOL_REPLY, AppendEventsRequest,
    AwaitEnvironmentReadyActivityRequest, AwaitEnvironmentReadyActivityResult,
    ContextCompactActivityRequest, CreateOrLoadSessionRequest, CreateOrLoadSessionResult,
    EnvironmentJobCancelActivityRequest, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult, EnvironmentJobStartActivityRequest,
    EnvironmentJobStartActivityResult, LlmGenerateActivityRequest,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult, RuntimeProjectionRefreshActivityRequest,
    RuntimeProjectionRefreshActivityResult, ToolInvokeBatchActivityRequest,
    ToolInvokeCallActivityRequest, ToolInvokeCallActivityResult,
    ToolPreparePromiseControlsActivityRequest,
};

mod common;
mod compaction;
mod environment_jobs;
mod llm;
mod preprocess;
mod runtime_projection;
pub use runtime_projection::subagent_catalog_snapshot;
mod state;
mod storage;
mod subagents;
mod tools;
mod workflow_tools;

pub use preprocess::{
    AudioTranscodeError, AudioTranscodeOutput, AudioTranscodeRequest, AudioTranscoder,
    AudioTranscriber, AudioTranscription, AudioTranscriptionError, AudioTranscriptionRequest,
    FfmpegAudioTranscoder, default_audio_transcoder_from_env,
};
pub use state::{
    ActivityState, LlmActivityDeps, PreprocessActivityDeps, RuntimeProjectionActivityDeps,
    StorageActivityDeps, ToolActivityDeps,
};

/// Worker-side universe routing. Activities carry no universe field; the
/// authoritative tenant identity is the composed workflow id
/// (`{universe_id}/{session_id}`, asserted at workflow bootstrap), which every
/// session activity task carries in its `ActivityContext`. System workflows
/// with content-derived ids instead pass an explicitly validated universe in
/// their activity input.
enum WorkerUniverses {
    /// One pre-built state for one universe. Used by tests and single-universe
    /// tools; activities for any other universe fail.
    Fixed {
        universe_id: uuid::Uuid,
        state: Arc<ActivityState>,
    },
    /// Lazy per-universe resolution over the deployment runtime. Never
    /// creates universes: a workflow for an unknown universe is a routing
    /// error, not a provisioning request.
    Runtime(Arc<UniverseRuntime>),
}

pub struct WorkerActivities {
    universes: WorkerUniverses,
}

impl WorkerActivities {
    /// Serve exactly one universe with an injected state (tests, fakes).
    pub fn for_universe(universe_id: uuid::Uuid, state: ActivityState) -> Self {
        Self {
            universes: WorkerUniverses::Fixed {
                universe_id,
                state: Arc::new(state),
            },
        }
    }

    /// Serve any universe of the deployment, resolving state lazily.
    pub fn with_runtime(runtime: Arc<UniverseRuntime>) -> Self {
        Self {
            universes: WorkerUniverses::Runtime(runtime),
        }
    }

    pub async fn from_env() -> anyhow::Result<Self> {
        let store = crate::config::pg_store_from_env().await?;
        let universe_id = store.config().universe_id;
        Ok(Self::for_universe(
            universe_id,
            ActivityState::from_pg_store_with_default_runtime(store)?,
        ))
    }

    pub fn from_pg_store_with_default_runtime(store: Arc<PgStore>) -> anyhow::Result<Self> {
        let universe_id = store.config().universe_id;
        Ok(Self::for_universe(
            universe_id,
            ActivityState::from_pg_store_with_default_runtime(store)?,
        ))
    }

    /// Resolve the universe of the invoking workflow from the activity
    /// context's workflow id and return that universe's activity state.
    async fn state_for(&self, ctx: &ActivityContext) -> Result<Arc<ActivityState>, ActivityError> {
        let workflow_id = ctx
            .info()
            .workflow_execution
            .as_ref()
            .map(|execution| execution.workflow_id.as_str())
            .ok_or_else(|| {
                ActivityError::application(ApplicationFailure::non_retryable(anyhow::anyhow!(
                    "activity task carries no workflow execution info"
                )))
            })?;
        let Some((universe_id, _session_id)) = temporal_workflow::split_workflow_id(workflow_id)
        else {
            return Err(ActivityError::application(
                ApplicationFailure::non_retryable(anyhow::anyhow!(
                    "workflow id is not universe-composed ({{universe_id}}/{{session_id}}): {workflow_id}"
                )),
            ));
        };
        self.state_for_universe(universe_id).await
    }

    async fn state_for_universe(
        &self,
        universe_id: uuid::Uuid,
    ) -> Result<Arc<ActivityState>, ActivityError> {
        match &self.universes {
            WorkerUniverses::Fixed {
                universe_id: served,
                state,
            } => {
                if *served != universe_id {
                    return Err(ActivityError::application(
                        ApplicationFailure::non_retryable(anyhow::anyhow!(
                            "worker serves universe {served} but activity requested {universe_id}"
                        )),
                    ));
                }
                Ok(state.clone())
            }
            WorkerUniverses::Runtime(runtime) => runtime
                .state_for(universe_id, false)
                .await
                .map(|state| state.activities.clone())
                .map_err(|error| match error {
                    UniverseError::Unknown { .. } => ActivityError::application(
                        ApplicationFailure::non_retryable(anyhow::anyhow!("{error}")),
                    ),
                    UniverseError::Runtime(_) => ActivityError::application(
                        ApplicationFailure::new(anyhow::anyhow!("{error}")),
                    ),
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        ContextSnapshot, CoreAgentLlm, CoreAgentTools, LlmGenerationRequest, LlmRequest,
        ModelSelection, ProviderApiKind, RunId, SessionId, ToolBatchId, ToolCallStatus,
        ToolInvocationBatchRequest, ToolInvocationRequest, TurnId,
        storage::{BlobStore, InMemoryBlobStore, InMemorySessionStore, SessionStore},
    };

    use crate::worker::{FAKE_TOOL_NAME, FakeLlm, FakeTools};

    use super::*;

    #[test]
    fn activity_names_match_workflow_definitions() {
        assert_eq!(
            WorkerActivities::create_or_load_session.name(),
            temporal_workflow::WorkflowActivities::create_or_load_session.name()
        );
        assert_eq!(
            WorkerActivities::put_blob.name(),
            temporal_workflow::WorkflowActivities::put_blob.name()
        );
        assert_eq!(
            WorkerActivities::read_blob.name(),
            temporal_workflow::WorkflowActivities::read_blob.name()
        );
        assert_eq!(
            WorkerActivities::materialize_await_result.name(),
            temporal_workflow::WorkflowActivities::materialize_await_result.name()
        );
        assert_eq!(
            WorkerActivities::append_events.name(),
            temporal_workflow::WorkflowActivities::append_events.name()
        );
        assert_eq!(
            WorkerActivities::llm_generate.name(),
            temporal_workflow::WorkflowActivities::llm_generate.name()
        );
        assert_eq!(
            WorkerActivities::preprocess_run_input.name(),
            temporal_workflow::WorkflowActivities::preprocess_run_input.name()
        );
        assert_eq!(
            WorkerActivities::context_compact.name(),
            temporal_workflow::WorkflowActivities::context_compact.name()
        );
        assert_eq!(
            WorkerActivities::tool_invoke_batch.name(),
            temporal_workflow::WorkflowActivities::tool_invoke_batch.name()
        );
        assert_eq!(
            WorkerActivities::tool_invoke_call.name(),
            temporal_workflow::WorkflowActivities::tool_invoke_call.name()
        );
        assert_eq!(
            WorkerActivities::await_environment_ready.name(),
            temporal_workflow::WorkflowActivities::await_environment_ready.name()
        );
        assert_eq!(
            WorkerActivities::tool_prepare_promise_controls.name(),
            temporal_workflow::WorkflowActivities::tool_prepare_promise_controls.name()
        );
        assert_eq!(
            WorkerActivities::runtime_projection_refresh.name(),
            temporal_workflow::WorkflowActivities::runtime_projection_refresh.name()
        );
        assert_eq!(
            WorkerActivities::environment_job_start.name(),
            temporal_workflow::WorkflowActivities::environment_job_start.name()
        );
        assert_eq!(
            WorkerActivities::environment_job_prepare_workflow_tool.name(),
            temporal_workflow::WorkflowActivities::environment_job_prepare_workflow_tool.name()
        );
        assert_eq!(
            WorkerActivities::environment_job_poll.name(),
            temporal_workflow::WorkflowActivities::environment_job_poll.name()
        );
        assert_eq!(
            WorkerActivities::environment_job_cancel.name(),
            temporal_workflow::WorkflowActivities::environment_job_cancel.name()
        );
        assert_eq!(
            WorkerActivities::validate_workflow_tool_reply.name(),
            temporal_workflow::WorkflowActivities::validate_workflow_tool_reply.name()
        );
        assert_eq!(
            WorkerActivities::start_workflow_tool_execution.name(),
            temporal_workflow::WorkflowActivities::start_workflow_tool_execution.name()
        );
        assert_eq!(
            WorkerActivities::check_workflow_tool_execution.name(),
            temporal_workflow::WorkflowActivities::check_workflow_tool_execution.name()
        );
        assert_eq!(
            WorkerActivities::cancel_workflow_tool_execution.name(),
            temporal_workflow::WorkflowActivities::cancel_workflow_tool_execution.name()
        );
        assert_eq!(
            WorkerActivities::subagent_prepare.name(),
            temporal_workflow::WorkflowActivities::subagent_prepare.name()
        );
        assert_eq!(
            WorkerActivities::subagent_resolve.name(),
            temporal_workflow::WorkflowActivities::subagent_resolve.name()
        );
        assert_eq!(
            WorkerActivities::subagent_close.name(),
            temporal_workflow::WorkflowActivities::subagent_close.name()
        );
    }

    #[test]
    fn process_timeout_ceiling_matches_worker_tool_limits() {
        // The workflow derives the process activity deadline from
        // PROCESS_TIMEOUT_CEILING while the worker clamps requested process
        // timeouts to ToolLimits::max_process_timeout_ms; the two bounds must
        // stay identical.
        assert_eq!(
            ::tools::limits::ToolLimits::default().max_process_timeout_ms,
            temporal_workflow::PROCESS_TIMEOUT_CEILING.as_millis() as u64
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn injected_fake_state_runs_llm_and_tools_without_env() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let session_store: Arc<dyn SessionStore> = sessions;
        let blob_store: Arc<dyn BlobStore> = blobs.clone();
        let llm = Arc::new(FakeLlm::new(blob_store.clone())) as Arc<dyn CoreAgentLlm>;
        let tools = Arc::new(FakeTools::new(blob_store.clone())) as Arc<dyn CoreAgentTools>;
        let state = ActivityState::new(session_store, blob_store, llm, tools);

        let generated = llm::generate(
            state.llm(),
            1,
            LlmGenerateActivityRequest {
                request: fake_llm_request(),
            },
        )
        .await
        .expect("generate fake tool call");
        let tool_call = generated.facts.tool_calls.first().expect("fake tool call");

        let invoked = tools::invoke_batch(
            state.tools(),
            ToolInvokeBatchActivityRequest {
                request: ToolInvocationBatchRequest {
                    vfs_working_directory: None,
                    session_id: SessionId::new("session-test"),
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                    batch_id: ToolBatchId::new(1),
                    promise_id_base: 1,
                    active_environment_id: None,
                    environment_policy: None,
                    subagents_policy: None,
                    workspace_links: Vec::new(),
                    calls: vec![ToolInvocationRequest {
                        builtin: None,
                        call_id: tool_call.call_id.clone(),
                        tool_id: Some(tool_call.tool_name.clone()),
                        tool_name: tool_call.tool_name.clone(),
                        arguments_ref: tool_call.arguments_ref.clone(),
                        workflow_tool: None,
                        promise_control: None,
                        remote_mcp: None,
                    }],
                },
            },
        )
        .await
        .expect("invoke fake tool");

        let invoked = invoked.completed_result().expect("completed tool batch");
        let result = invoked.results.first().expect("tool result");
        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let output_ref = result.output_ref.as_ref().expect("output ref");
        let output = blobs.read_text(output_ref).await.expect("tool output");
        assert!(output.contains(FAKE_TOOL_NAME));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transient_llm_errors_become_typed_retryable_activity_failures() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let session_store: Arc<dyn SessionStore> = sessions;
        let blob_store: Arc<dyn BlobStore> = blobs.clone();
        let llm = Arc::new(FakeLlm::new(blob_store.clone()).with_transient_failures(1))
            as Arc<dyn CoreAgentLlm>;
        let tools = Arc::new(FakeTools::new(blob_store.clone())) as Arc<dyn CoreAgentTools>;
        let state = ActivityState::new(session_store, blob_store, llm, tools);

        let error = llm::generate(
            state.llm(),
            3,
            LlmGenerateActivityRequest {
                request: fake_llm_request(),
            },
        )
        .await
        .expect_err("transient provider errors must fail the activity so Temporal retries it");
        let ActivityError::Application(failure) = error else {
            panic!("transient provider errors must surface as application failures");
        };
        assert_eq!(
            failure.type_name(),
            Some(temporal_workflow::LLM_PROVIDER_TRANSIENT_ERROR_TYPE)
        );
        assert!(!failure.is_non_retryable());
        assert_eq!(
            failure.next_retry_delay(),
            Some(crate::worker::FAKE_TRANSIENT_RETRY_AFTER)
        );

        // The scripted budget is consumed: the next attempt succeeds.
        let generated = llm::generate(
            state.llm(),
            4,
            LlmGenerateActivityRequest {
                request: fake_llm_request(),
            },
        )
        .await
        .expect("post-transient attempt succeeds");
        assert!(!generated.facts.tool_calls.is_empty());
    }

    fn fake_llm_request() -> LlmGenerationRequest {
        LlmGenerationRequest {
            session_id: SessionId::new("session-test"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: LlmRequest {
                model: ModelSelection {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    provider_id: "fake".to_owned(),
                    model: "fake-agent".to_owned(),
                },
                request_fingerprint: "fake-agent-test".to_owned(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 0,
                    entries: Vec::new(),
                    token_estimate: None,
                },
                tools: vec![engine::ToolSpec {
                    name: engine::ToolName::new(FAKE_TOOL_NAME),
                    execution: Default::default(),
                    kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                        description_ref: None,
                        input_schema_ref: engine::BlobRef::from_bytes(
                            br#"{"type":"object","properties":{"text":{"type":"string"}}}"#,
                        ),
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: engine::ToolParallelism::ParallelSafe,
                }],
                tool_choice: None,
                reasoning_effort: None,
                parallel_tool_use: None,
                processing_tier: None,
                output_limit: None,
                provider_response_id: None,
                compaction: None,
                params: None,
            },
        }
    }
}

#[activities]
impl WorkerActivities {
    #[activity(name = ACTIVITY_CREATE_OR_LOAD_SESSION)]
    pub async fn create_or_load_session(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CreateOrLoadSessionRequest,
    ) -> Result<CreateOrLoadSessionResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        storage::create_or_load_session(state.storage(), request).await
    }

    #[activity(name = ACTIVITY_PUT_BLOB)]
    pub async fn put_blob(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: PutBlobRequest,
    ) -> Result<BlobRef, ActivityError> {
        let state = self.state_for(&ctx).await?;
        storage::put_blob(state.storage(), request).await
    }

    #[activity(name = ACTIVITY_READ_BLOB)]
    pub async fn read_blob(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: ReadBlobRequest,
    ) -> Result<ReadBlobResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        storage::read_blob(state.storage(), request).await
    }

    #[activity(name = ACTIVITY_MATERIALIZE_AWAIT_RESULT)]
    pub async fn materialize_await_result(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: temporal_workflow::AwaitMaterializationRequest,
    ) -> Result<BlobRef, ActivityError> {
        let state = self.state_for(&ctx).await?;
        storage::materialize_await_result(state.storage(), request).await
    }

    #[activity(name = ACTIVITY_APPEND_EVENTS)]
    pub async fn append_events(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: AppendEventsRequest,
    ) -> Result<engine::storage::AppendSessionEventsResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        storage::append_events(state.storage(), request).await
    }

    #[activity(name = ACTIVITY_LLM_GENERATE)]
    pub async fn llm_generate(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: LlmGenerateActivityRequest,
    ) -> Result<LlmGenerationResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        let attempt = ctx.info().attempt;
        common::cancellable(&ctx, llm::generate(state.llm(), attempt, request)).await
    }

    #[activity(name = ACTIVITY_PREPROCESS_RUN_INPUT)]
    pub async fn preprocess_run_input(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: PreprocessRunInputActivityRequest,
    ) -> Result<PreprocessRunInputActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        preprocess::preprocess_run_input(state.preprocess(), request).await
    }

    #[activity(name = ACTIVITY_CONTEXT_COMPACT)]
    pub async fn context_compact(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: ContextCompactActivityRequest,
    ) -> Result<ContextCompactionResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        let attempt = ctx.info().attempt;
        common::cancellable(
            &ctx,
            compaction::compact_context(state.llm(), attempt, request),
        )
        .await
    }

    #[activity(name = ACTIVITY_TOOL_INVOKE_BATCH)]
    pub async fn tool_invoke_batch(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: ToolInvokeBatchActivityRequest,
    ) -> Result<ToolBatchOutcome, ActivityError> {
        let state = self.state_for(&ctx).await?;
        common::cancellable(&ctx, tools::invoke_batch(state.tools(), request)).await
    }

    #[activity(name = ACTIVITY_TOOL_INVOKE_CALL)]
    pub async fn tool_invoke_call(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: ToolInvokeCallActivityRequest,
    ) -> Result<ToolInvokeCallActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        common::cancellable(&ctx, tools::invoke_call(state.tools(), request)).await
    }

    #[activity(name = ACTIVITY_AWAIT_ENVIRONMENT_READY)]
    pub async fn await_environment_ready(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: AwaitEnvironmentReadyActivityRequest,
    ) -> Result<AwaitEnvironmentReadyActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        tools::await_environment_ready(state.tools(), &ctx, request).await
    }

    #[activity(name = ACTIVITY_TOOL_PREPARE_PROMISE_CONTROLS)]
    pub async fn tool_prepare_promise_controls(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: ToolPreparePromiseControlsActivityRequest,
    ) -> Result<engine::PromiseControlArgumentFacts, ActivityError> {
        let state = self.state_for(&ctx).await?;
        tools::prepare_promise_controls(state.tools().blobs.as_ref(), request.request).await
    }

    #[activity(name = ACTIVITY_RUNTIME_PROJECTION_REFRESH)]
    pub async fn runtime_projection_refresh(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: RuntimeProjectionRefreshActivityRequest,
    ) -> Result<RuntimeProjectionRefreshActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        runtime_projection::refresh_runtime_projection(state.runtime_projection(), request).await
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_START)]
    pub async fn environment_job_start(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: EnvironmentJobStartActivityRequest,
    ) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
        let state = self.state_for_universe(request.universe_id).await?;
        environment_jobs::start(state.environment_jobs(), request).await
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_PREPARE_WORKFLOW_TOOL)]
    pub async fn environment_job_prepare_workflow_tool(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: temporal_workflow::EnvironmentJobPrepareWorkflowToolRequest,
    ) -> Result<temporal_workflow::EnvironmentJobWorkflowArgs, ActivityError> {
        let state = self.state_for_universe(request.start.universe_id).await?;
        environment_jobs::prepare_workflow_tool(state.environment_jobs(), request).await
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_POLL)]
    pub async fn environment_job_poll(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: EnvironmentJobPollActivityRequest,
    ) -> Result<EnvironmentJobPollActivityResult, ActivityError> {
        let state = self.state_for_universe(request.universe_id).await?;
        environment_jobs::poll(state.environment_jobs(), request).await
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_CANCEL)]
    pub async fn environment_job_cancel(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: EnvironmentJobCancelActivityRequest,
    ) -> Result<Vec<environment_protocol::data::jobs::JobSummary>, ActivityError> {
        let state = self.state_for_universe(request.universe_id).await?;
        environment_jobs::cancel(state.environment_jobs(), request).await
    }

    #[activity(name = ACTIVITY_VALIDATE_WORKFLOW_TOOL_REPLY)]
    pub async fn validate_workflow_tool_reply(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: temporal_workflow::WorkflowToolReplyValidationRequest,
    ) -> Result<temporal_workflow::WorkflowToolReplyValidationResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        storage::validate_workflow_tool_reply(state.storage(), request).await
    }

    #[activity(name = ACTIVITY_START_WORKFLOW_TOOL_EXECUTION)]
    pub async fn start_workflow_tool_execution(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: temporal_workflow::WorkflowToolStartActivityRequest,
    ) -> Result<temporal_workflow::WorkflowToolStartActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        workflow_tools::start_execution(state.workflow_tool_executions(), state.storage(), request)
            .await
    }

    #[activity(name = ACTIVITY_CHECK_WORKFLOW_TOOL_EXECUTION)]
    pub async fn check_workflow_tool_execution(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: temporal_workflow::WorkflowToolExecutionCheckRequest,
    ) -> Result<PromiseSourceCheckResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        workflow_tools::check_execution(state.workflow_tool_executions(), state.storage(), request)
            .await
    }

    #[activity(name = ACTIVITY_CANCEL_WORKFLOW_TOOL_EXECUTION)]
    pub async fn cancel_workflow_tool_execution(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: temporal_workflow::WorkflowToolExecutionCancelRequest,
    ) -> Result<(), ActivityError> {
        let state = self.state_for(&ctx).await?;
        workflow_tools::cancel_execution(state.workflow_tool_executions(), request).await
    }

    #[activity(name = ACTIVITY_SUBAGENT_PREPARE)]
    pub async fn subagent_prepare(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: temporal_workflow::SubagentPrepareActivityRequest,
    ) -> Result<temporal_workflow::SubagentPrepareActivityResult, ActivityError> {
        let state = self.state_for_universe(request.start.universe_id).await?;
        subagents::prepare(state.subagents(), request).await
    }

    #[activity(name = ACTIVITY_SUBAGENT_RESOLVE)]
    pub async fn subagent_resolve(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: temporal_workflow::SubagentResolveActivityRequest,
    ) -> Result<engine::PromiseResolution, ActivityError> {
        let state = self.state_for_universe(request.universe_id).await?;
        subagents::resolve(state.subagents(), request).await
    }

    #[activity(name = ACTIVITY_SUBAGENT_CLOSE)]
    pub async fn subagent_close(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: temporal_workflow::SubagentCloseActivityRequest,
    ) -> Result<(), ActivityError> {
        let state = self.state_for_universe(request.universe_id).await?;
        subagents::close(state.subagents(), request).await
    }
}
