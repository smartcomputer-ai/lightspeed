use engine::{BlobRef, ContextCompactionResult, LlmGenerationResult, ToolBatchOutcome};
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::{
    AppendEventsRequest, ContextCompactActivityRequest, CreateOrLoadSessionRequest,
    CreateOrLoadSessionResult, LlmGenerateActivityRequest, PreprocessRunInputActivityRequest,
    PreprocessRunInputActivityResult, PutBlobRequest, ReadBlobRequest, ReadBlobResult,
    SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult,
    ToolInvokeBatchActivityRequest,
};

pub const ACTIVITY_CREATE_OR_LOAD_SESSION: &str = "WorkflowActivities::create_or_load_session";
pub const ACTIVITY_PUT_BLOB: &str = "WorkflowActivities::put_blob";
pub const ACTIVITY_READ_BLOB: &str = "WorkflowActivities::read_blob";
pub const ACTIVITY_APPEND_EVENTS: &str = "WorkflowActivities::append_events";
pub const ACTIVITY_LLM_GENERATE: &str = "WorkflowActivities::llm_generate";
pub const ACTIVITY_PREPROCESS_RUN_INPUT: &str = "WorkflowActivities::preprocess_run_input";
pub const ACTIVITY_CONTEXT_COMPACT: &str = "WorkflowActivities::context_compact";
pub const ACTIVITY_TOOL_INVOKE_BATCH: &str = "WorkflowActivities::tool_invoke_batch";
pub const ACTIVITY_SKILL_CATALOG_REFRESH: &str = "WorkflowActivities::skill_catalog_refresh";

pub struct WorkflowActivities;

#[activities]
impl WorkflowActivities {
    #[activity(name = ACTIVITY_CREATE_OR_LOAD_SESSION)]
    pub async fn create_or_load_session(
        _ctx: ActivityContext,
        _request: CreateOrLoadSessionRequest,
    ) -> Result<CreateOrLoadSessionResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_PUT_BLOB)]
    pub async fn put_blob(
        _ctx: ActivityContext,
        _request: PutBlobRequest,
    ) -> Result<BlobRef, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_READ_BLOB)]
    pub async fn read_blob(
        _ctx: ActivityContext,
        _request: ReadBlobRequest,
    ) -> Result<ReadBlobResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_APPEND_EVENTS)]
    pub async fn append_events(
        _ctx: ActivityContext,
        _request: AppendEventsRequest,
    ) -> Result<engine::storage::AppendSessionEventsResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_LLM_GENERATE)]
    pub async fn llm_generate(
        _ctx: ActivityContext,
        _request: LlmGenerateActivityRequest,
    ) -> Result<LlmGenerationResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_PREPROCESS_RUN_INPUT)]
    pub async fn preprocess_run_input(
        _ctx: ActivityContext,
        _request: PreprocessRunInputActivityRequest,
    ) -> Result<PreprocessRunInputActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_CONTEXT_COMPACT)]
    pub async fn context_compact(
        _ctx: ActivityContext,
        _request: ContextCompactActivityRequest,
    ) -> Result<ContextCompactionResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_TOOL_INVOKE_BATCH)]
    pub async fn tool_invoke_batch(
        _ctx: ActivityContext,
        _request: ToolInvokeBatchActivityRequest,
    ) -> Result<ToolBatchOutcome, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_SKILL_CATALOG_REFRESH)]
    pub async fn skill_catalog_refresh(
        _ctx: ActivityContext,
        _request: SkillCatalogRefreshActivityRequest,
    ) -> Result<SkillCatalogRefreshActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }
}
