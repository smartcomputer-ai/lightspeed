use engine::{BlobRef, LlmGenerationResult, ToolInvocationBatchResult};
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::{
    AppendEventsRequest, CreateOrLoadSessionRequest, CreateOrLoadSessionResult,
    LlmGenerateActivityRequest, PutBlobRequest, ReadBlobRequest, ReadBlobResult,
    ToolInvokeBatchActivityRequest,
};

pub const ACTIVITY_CREATE_OR_LOAD_SESSION: &str = "WorkflowActivities::create_or_load_session";
pub const ACTIVITY_PUT_BLOB: &str = "WorkflowActivities::put_blob";
pub const ACTIVITY_READ_BLOB: &str = "WorkflowActivities::read_blob";
pub const ACTIVITY_APPEND_EVENTS: &str = "WorkflowActivities::append_events";
pub const ACTIVITY_LLM_GENERATE: &str = "WorkflowActivities::llm_generate";
pub const ACTIVITY_TOOL_INVOKE_BATCH: &str = "WorkflowActivities::tool_invoke_batch";

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

    #[activity(name = ACTIVITY_TOOL_INVOKE_BATCH)]
    pub async fn tool_invoke_batch(
        _ctx: ActivityContext,
        _request: ToolInvokeBatchActivityRequest,
    ) -> Result<ToolInvocationBatchResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }
}
