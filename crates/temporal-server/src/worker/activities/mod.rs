use std::sync::Arc;

use engine::{BlobRef, ContextCompactionResult, LlmGenerationResult, ToolBatchOutcome};
use store_pg::PgStore;
use temporalio_common::error::ApplicationFailure;
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::fleet::FleetChildRuntime;
use crate::universe::{UniverseError, UniverseRuntime};
use crate::worker::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CHECK_ENVIRONMENT_JOB_WAIT, ACTIVITY_CONTEXT_COMPACT,
    ACTIVITY_CREATE_OR_LOAD_SESSION, ACTIVITY_LLM_GENERATE, ACTIVITY_PREPROCESS_RUN_INPUT,
    ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB, ACTIVITY_SKILL_CATALOG_REFRESH,
    ACTIVITY_TOOL_INVOKE_BATCH, AppendEventsRequest, CheckEnvironmentJobWaitActivityRequest,
    CheckEnvironmentJobWaitActivityResult, ContextCompactActivityRequest,
    CreateOrLoadSessionRequest, CreateOrLoadSessionResult, LlmGenerateActivityRequest,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult, SkillCatalogRefreshActivityRequest,
    SkillCatalogRefreshActivityResult, ToolInvokeBatchActivityRequest,
};

mod common;
mod compaction;
mod llm;
mod preprocess;
mod skills;
mod state;
mod storage;
mod tools;

pub use preprocess::{
    AudioTranscodeError, AudioTranscodeOutput, AudioTranscodeRequest, AudioTranscoder,
    AudioTranscriber, AudioTranscription, AudioTranscriptionError, AudioTranscriptionRequest,
    FfmpegAudioTranscoder, default_audio_transcoder_from_env,
};
pub use state::{
    ActivityState, LlmActivityDeps, PreprocessActivityDeps, SkillCatalogActivityDeps,
    StorageActivityDeps, ToolActivityDeps,
};

/// Worker-side universe routing. Activities carry no universe field; the
/// authoritative tenant identity is the composed workflow id
/// (`{universe_id}/{session_id}`, asserted at workflow bootstrap), which every
/// activity task carries in its `ActivityContext`.
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

    pub fn from_pg_store_with_default_runtime_and_fleet(
        store: Arc<PgStore>,
        fleet_runtime: Arc<dyn FleetChildRuntime>,
    ) -> anyhow::Result<Self> {
        let universe_id = store.config().universe_id;
        Ok(Self::for_universe(
            universe_id,
            ActivityState::from_pg_store_with_default_runtime_and_fleet(store, fleet_runtime)?,
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
        match &self.universes {
            WorkerUniverses::Fixed {
                universe_id: served,
                state,
            } => {
                if *served != universe_id {
                    return Err(ActivityError::application(
                        ApplicationFailure::non_retryable(anyhow::anyhow!(
                            "worker serves universe {served} but workflow {workflow_id} belongs to {universe_id}"
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
            WorkerActivities::skill_catalog_refresh.name(),
            temporal_workflow::WorkflowActivities::skill_catalog_refresh.name()
        );
        assert_eq!(
            WorkerActivities::check_environment_job_wait.name(),
            temporal_workflow::WorkflowActivities::check_environment_job_wait.name()
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
                    session_id: SessionId::new("session-test"),
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                    batch_id: ToolBatchId::new(1),
                    default_targets: Default::default(),
                    calls: vec![ToolInvocationRequest {
                        call_id: tool_call.call_id.clone(),
                        tool_name: tool_call.tool_name.clone(),
                        arguments_ref: tool_call.arguments_ref.clone(),
                        execution_target: None,
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
                    kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                        model_name: None,
                        description_ref: None,
                        input_schema_ref: engine::BlobRef::from_bytes(
                            br#"{"type":"object","properties":{"text":{"type":"string"}}}"#,
                        ),
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: engine::ToolParallelism::ParallelSafe,
                    target_requirement: engine::ToolTargetRequirement::None,
                }],
                tool_choice: None,
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
        llm::generate(state.llm(), request).await
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
        compaction::compact_context(state.llm(), request).await
    }

    #[activity(name = ACTIVITY_TOOL_INVOKE_BATCH)]
    pub async fn tool_invoke_batch(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: ToolInvokeBatchActivityRequest,
    ) -> Result<ToolBatchOutcome, ActivityError> {
        let state = self.state_for(&ctx).await?;
        tools::invoke_batch(state.tools(), request).await
    }

    #[activity(name = ACTIVITY_SKILL_CATALOG_REFRESH)]
    pub async fn skill_catalog_refresh(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: SkillCatalogRefreshActivityRequest,
    ) -> Result<SkillCatalogRefreshActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        skills::refresh_skill_catalog(state.skill_catalog(), request).await
    }

    #[activity(name = ACTIVITY_CHECK_ENVIRONMENT_JOB_WAIT)]
    pub async fn check_environment_job_wait(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CheckEnvironmentJobWaitActivityRequest,
    ) -> Result<CheckEnvironmentJobWaitActivityResult, ActivityError> {
        let state = self.state_for(&ctx).await?;
        tools::check_environment_job_wait(state.tools(), request).await
    }
}
