//! Temporal workflow contract and deterministic session orchestration.

mod activities;
mod config;
mod rehydrate;
mod temporal_helpers;
mod types;
mod workflow;

pub use activities::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION,
    ACTIVITY_LLM_GENERATE, ACTIVITY_PREPROCESS_RUN_INPUT, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB,
    ACTIVITY_SKILL_CATALOG_REFRESH, ACTIVITY_TOOL_INVOKE_BATCH, WorkflowActivities,
};
pub use config::{
    DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES, DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
    DEFAULT_MODEL, DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET,
    FAKE_TOOL_NAME, activity_options, default_instructions, default_run_config,
    default_session_config,
};
pub use rehydrate::{ReducedSession, RehydrateError, reduce_session_entries};
pub use temporal_helpers::connect_temporal;
pub use types::{
    ActiveWaitRecord, ActiveWaitSubscription, AgentActiveRunSummary, AgentAdmission,
    AgentAdmissionFailure, AgentAdmissionFailureKind, AgentCompletedRunSummary,
    AgentQueuedRunSummary, AgentSessionArgs, AgentSessionStatus, AgentWaitDirective,
    AgentWaitHandle, AgentWaitHandleResult, AgentWaitHandleStatus, AgentWaitMode, AgentWaitOutcome,
    AgentWaitOutput, AgentWaitRunResult, AppendEventsRequest, ContextCompactActivityRequest,
    CreateOrLoadSessionRequest, CreateOrLoadSessionResult, FLEET_AGENT_WAIT_DIRECTIVE_KIND,
    LlmGenerateActivityRequest, PendingRunTerminalNotification, PendingToolBatchResume,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PreprocessRunInputFailure,
    PreprocessRunInputFailureKind, PreprocessRunInputOutcome, PutBlobRequest, ReadBlobRequest,
    ReadBlobResult, RunSubscription, RunTerminalNotification, SessionBootstrapPayloadTooLarge,
    SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult,
    ToolInvokeBatchActivityRequest,
};
pub use workflow::AgentSessionWorkflow;
