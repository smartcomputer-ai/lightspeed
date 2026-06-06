//! Temporal workflow contract and deterministic session orchestration.

mod activities;
mod config;
mod temporal_helpers;
mod types;
mod workflow;

pub use activities::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION,
    ACTIVITY_LLM_GENERATE, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB, ACTIVITY_SKILL_CATALOG_REFRESH,
    ACTIVITY_TOOL_INVOKE_BATCH, WorkflowActivities,
};
pub use config::{
    DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD, DEFAULT_MODEL, DEFAULT_TASK_QUEUE,
    DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, FAKE_TOOL_NAME, activity_options,
    default_instructions, default_run_config, default_session_config,
};
pub use temporal_helpers::connect_temporal;
pub use types::{
    AgentActiveRunSummary, AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind,
    AgentCompletedRunSummary, AgentQueuedRunSummary, AgentSessionArgs, AgentSessionStatus,
    AppendEventsRequest, ContextCompactActivityRequest, CreateOrLoadSessionRequest,
    CreateOrLoadSessionResult, LlmGenerateActivityRequest, PutBlobRequest, ReadBlobRequest,
    ReadBlobResult, SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult,
    ToolInvokeBatchActivityRequest,
};
pub use workflow::AgentSessionWorkflow;
