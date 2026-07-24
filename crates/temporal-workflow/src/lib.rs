//! Temporal workflow contract and deterministic session orchestration.

mod activities;
mod config;
mod rehydrate;
mod temporal_helpers;
mod types;
mod workflows;

pub use activities::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_CANCEL_PROMISE_SOURCE, ACTIVITY_CHECK_PROMISE_SOURCE,
    ACTIVITY_CONTEXT_COMPACT, ACTIVITY_CREATE_OR_LOAD_SESSION, ACTIVITY_ENVIRONMENT_JOB_CANCEL,
    ACTIVITY_ENVIRONMENT_JOB_POLL, ACTIVITY_ENVIRONMENT_JOB_START, ACTIVITY_LLM_GENERATE,
    ACTIVITY_PREPROCESS_RUN_INPUT, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB,
    ACTIVITY_RUNTIME_PROJECTION_REFRESH, ACTIVITY_SUBSCRIBE_PROMISE_SOURCE,
    ACTIVITY_TOOL_INVOKE_BATCH, WorkflowActivities,
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
    AgentActiveRunSummary, AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind,
    AgentCompletedRunSummary, AgentMessageSubmissionConsumptionSummary, AgentQueuedRunSummary,
    AgentSessionArgs, AgentSessionStatus, AppendEventsRequest, AwaitOutcome, AwaitOutput,
    AwaitPromiseResult, CancellingWatchdog, ContextCompactActivityRequest,
    CreateOrLoadSessionRequest, CreateOrLoadSessionResult, EnvironmentJobCancelActivityRequest,
    EnvironmentJobCancelSignal, EnvironmentJobConfirmSubscriptionSignal,
    EnvironmentJobCredentialScope, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult, EnvironmentJobProvenance, EnvironmentJobStartActivityRequest,
    EnvironmentJobStartActivityResult, EnvironmentJobStartPayload, EnvironmentJobSubscription,
    EnvironmentJobWorkflowArgs, EnvironmentJobWorkflowSnapshot, LlmGenerateActivityRequest,
    PendingEmission, PendingPromiseCancellation, PendingToolBatchResume,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PreprocessRunInputFailure,
    PreprocessRunInputFailureKind, PreprocessRunInputOutcome, PromiseSourcePoll, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult, RuntimeProjectionRefreshActivityRequest,
    RuntimeProjectionRefreshActivityResult, SessionBootstrapPayloadTooLarge,
    ToolInvokeBatchActivityRequest, compose_environment_job_workflow_id, compose_workflow_id,
    split_workflow_id,
};
pub use workflows::{AgentSessionWorkflow, EnvironmentJobWorkflow};
