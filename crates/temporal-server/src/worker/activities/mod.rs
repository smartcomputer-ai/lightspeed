use std::sync::Arc;

use engine::{BlobRef, ContextCompactionResult, LlmGenerationResult, ToolInvocationBatchResult};
use store_pg::PgStore;
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::worker::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION,
    ACTIVITY_LLM_GENERATE, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB, ACTIVITY_SKILL_CATALOG_REFRESH,
    ACTIVITY_TOOL_INVOKE_BATCH, AppendEventsRequest, ContextCompactActivityRequest,
    CreateOrLoadSessionRequest, CreateOrLoadSessionResult, LlmGenerateActivityRequest,
    PutBlobRequest, ReadBlobRequest, ReadBlobResult, SkillCatalogRefreshActivityRequest,
    SkillCatalogRefreshActivityResult, ToolInvokeBatchActivityRequest,
};

mod common;
mod compaction;
mod llm;
mod skills;
mod state;
mod storage;
mod tools;

pub use state::{
    ActivityState, LlmActivityDeps, SkillCatalogActivityDeps, StorageActivityDeps, ToolActivityDeps,
};

pub struct WorkerActivities {
    state: Arc<ActivityState>,
}

impl WorkerActivities {
    pub fn new(state: ActivityState) -> Self {
        Self {
            state: Arc::new(state),
        }
    }

    pub async fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(ActivityState::from_env().await?))
    }

    pub fn from_pg_store_with_default_runtime(store: Arc<PgStore>) -> anyhow::Result<Self> {
        Ok(Self::new(
            ActivityState::from_pg_store_with_default_runtime(store)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        ContextSnapshot, CoreAgentLlm, CoreAgentTools, LlmGenerationRequest, LlmRequest,
        ModelSelection, ProviderApiKind, RunId, SessionId, ToolBatchId,
        ToolCallStatus, ToolInvocationBatchRequest, ToolInvocationRequest, TurnId,
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
        _ctx: ActivityContext,
        request: CreateOrLoadSessionRequest,
    ) -> Result<CreateOrLoadSessionResult, ActivityError> {
        storage::create_or_load_session(self.state.storage(), request).await
    }

    #[activity(name = ACTIVITY_PUT_BLOB)]
    pub async fn put_blob(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: PutBlobRequest,
    ) -> Result<BlobRef, ActivityError> {
        storage::put_blob(self.state.storage(), request).await
    }

    #[activity(name = ACTIVITY_READ_BLOB)]
    pub async fn read_blob(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ReadBlobRequest,
    ) -> Result<ReadBlobResult, ActivityError> {
        storage::read_blob(self.state.storage(), request).await
    }

    #[activity(name = ACTIVITY_APPEND_EVENTS)]
    pub async fn append_events(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: AppendEventsRequest,
    ) -> Result<engine::storage::AppendSessionEventsResult, ActivityError> {
        storage::append_events(self.state.storage(), request).await
    }

    #[activity(name = ACTIVITY_LLM_GENERATE)]
    pub async fn llm_generate(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: LlmGenerateActivityRequest,
    ) -> Result<LlmGenerationResult, ActivityError> {
        llm::generate(self.state.llm(), request).await
    }

    #[activity(name = ACTIVITY_CONTEXT_COMPACT)]
    pub async fn context_compact(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ContextCompactActivityRequest,
    ) -> Result<ContextCompactionResult, ActivityError> {
        compaction::compact_context(self.state.llm(), request).await
    }

    #[activity(name = ACTIVITY_TOOL_INVOKE_BATCH)]
    pub async fn tool_invoke_batch(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: ToolInvokeBatchActivityRequest,
    ) -> Result<ToolInvocationBatchResult, ActivityError> {
        tools::invoke_batch(self.state.tools(), request).await
    }

    #[activity(name = ACTIVITY_SKILL_CATALOG_REFRESH)]
    pub async fn skill_catalog_refresh(
        self: Arc<Self>,
        _ctx: ActivityContext,
        request: SkillCatalogRefreshActivityRequest,
    ) -> Result<SkillCatalogRefreshActivityResult, ActivityError> {
        skills::refresh_skill_catalog(self.state.skill_catalog(), request).await
    }
}
