//! Temporal workflow contract and deterministic session orchestration.

mod activities;
mod code_execution;
mod code_tools;
pub use code_tools::*;
mod config;
mod environment_jobs;
pub use environment_jobs::{
    EnvironmentJobCancelActivityRequest, EnvironmentJobCancelActivityResult,
    EnvironmentJobCancelScope, EnvironmentJobCancelSignal, EnvironmentJobCommand,
    EnvironmentJobDependency, EnvironmentJobDependencyPolicy, EnvironmentJobId,
    EnvironmentJobObservation, EnvironmentJobPollActivityRequest, EnvironmentJobPollActivityResult,
    EnvironmentJobPrepareWorkflowToolRequest, EnvironmentJobRecord, EnvironmentJobSpec,
    EnvironmentJobStartActivityRequest, EnvironmentJobStartActivityResult,
    EnvironmentJobStartPayloadError, EnvironmentJobStartPayloadV1, EnvironmentJobStatus,
    EnvironmentJobSubscription, EnvironmentJobWorkflowArgs, EnvironmentJobWorkflowInput,
    EnvironmentJobWorkflowSnapshot, EnvironmentJobWorkflowToolContext,
};
mod mcp_approval;
pub use mcp_approval::provider_mcp_approval_response;
mod rehydrate;
mod session_preparation;
mod temporal_helpers;
mod types;
pub use activities::{ACTIVITY_CODE_TOOL_INVOKE, ACTIVITY_CODE_TOOL_PREPARE_SCOPE};
pub use session_preparation::*;
pub use types::{
    CodeToolInvokeActivityRequest, CodeToolInvokeActivityResult,
    CodeToolPrepareScopeActivityRequest, CodeToolPrepareScopeActivityResult,
};
pub mod workflow_contract;
mod workflows;

pub use activities::{
    ACTIVITY_APPEND_EVENTS, ACTIVITY_AWAIT_ENVIRONMENT_READY,
    ACTIVITY_CANCEL_WORKFLOW_TOOL_EXECUTION, ACTIVITY_CHECK_WORKFLOW_TOOL_EXECUTION,
    ACTIVITY_CODE_FINALIZE, ACTIVITY_CODE_PREPARE, ACTIVITY_CODE_RUN, ACTIVITY_CONTEXT_COMPACT,
    ACTIVITY_CREATE_OR_LOAD_SESSION, ACTIVITY_ENVIRONMENT_JOB_CANCEL,
    ACTIVITY_ENVIRONMENT_JOB_POLL, ACTIVITY_ENVIRONMENT_JOB_PREPARE_WORKFLOW_TOOL,
    ACTIVITY_ENVIRONMENT_JOB_START, ACTIVITY_LLM_GENERATE, ACTIVITY_MATERIALIZE_AWAIT_RESULT,
    ACTIVITY_PREPARE_JOINED_CONTEXT, ACTIVITY_PUT_BLOB, ACTIVITY_READ_BLOB,
    ACTIVITY_RUNTIME_PROJECTION_REFRESH, ACTIVITY_START_WORKFLOW_TOOL_EXECUTION,
    ACTIVITY_SUBAGENT_CLOSE, ACTIVITY_SUBAGENT_LIVENESS, ACTIVITY_SUBAGENT_PREPARE,
    ACTIVITY_SUBAGENT_RESOLVE, ACTIVITY_TOOL_INVOKE_BATCH, ACTIVITY_TOOL_INVOKE_CALL,
    ACTIVITY_TOOL_PREPARE_PROMISE_CONTROLS, ACTIVITY_VALIDATE_WORKFLOW_TOOL_REPLY,
    WorkflowActivities,
};
pub use code_execution::{
    CODE_EXECUTION_WORKFLOW_TYPE, CodeExecutionDescriptor, CodeExecutionInterruption,
    CodeExecutionLimits, CodeExecutionPhase, CodeExecutionSnapshot, CodeExecutionTerminal,
    CodeExecutionValidationError, CodeFinalizeActivityRequest, CodePrepareActivityRequest,
    CodePrepareActivityResult, CodeRunActivityResult,
};
pub use config::{
    ACTIVITY_CANCELLATION_HEARTBEAT_INTERVAL, ACTIVITY_CANCELLATION_HEARTBEAT_TIMEOUT,
    CODE_TOOL_PREPARE_SCOPE_SCHEDULE_TO_CLOSE, DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES,
    DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD, DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, ENVIRONMENT_READY_GRACE, ENVIRONMENT_READY_HEARTBEAT_TIMEOUT,
    ENVIRONMENT_READY_POLL_INTERVAL, ENVIRONMENT_READY_WAIT, FAKE_TOOL_NAME,
    LLM_RETRY_MAX_ATTEMPTS, LLM_RETRY_MAX_INTERVAL, LLM_SCHEDULE_TO_CLOSE, LLM_START_TO_CLOSE,
    MAX_CONCURRENT_TOOL_CALLS_PER_BATCH, PROCESS_TIMEOUT_CEILING,
    SUBAGENT_ACTIVITY_SCHEDULE_TO_CLOSE, SUBAGENT_LIVENESS_CHECK_INTERVAL,
    TOOL_INTERACTIVE_OPERATION_TIMEOUT, TOOL_PROCESS_GRACE, TOOL_REMOTE_OPERATION_TIMEOUT,
    TOOL_RETRY_SAFE_MAX_ATTEMPTS, activity_options, boundary_error_blob_activity_options,
    code_tool_prepare_scope_activity_options, default_instructions, default_run_config,
    default_session_config, environment_ready_activity_options, llm_activity_options,
    recovery_poll_activity_options, subagent_activity_options, tool_batch_activity_options,
    tool_call_activity_options, tool_call_operation_timeout,
};
pub use rehydrate::{
    ReducedSession, RehydrateError, reduce_session_entries, reduce_session_entries_from,
};
pub use temporal_helpers::{
    TEMPORAL_ADDRESS_ENV, TEMPORAL_API_KEY_ENV, TEMPORAL_API_KEY_FILE_ENV, TEMPORAL_NAMESPACE_ENV,
    TEMPORAL_TLS_CLIENT_CERT_FILE_ENV, TEMPORAL_TLS_CLIENT_KEY_FILE_ENV, TEMPORAL_TLS_ENV,
    TEMPORAL_TLS_SERVER_CA_FILE_ENV, TEMPORAL_TLS_SERVER_NAME_ENV, TemporalClientCertificate,
    TemporalConfigError, TemporalConnectionConfig, TemporalConnectionSettings, TemporalTlsConfig,
    connect_temporal,
};
pub use types::{
    AgentActiveRunSummary, AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind,
    AgentFinishedRunSummary, AgentQueuedRunSummary, AgentSessionArgs,
    AgentSessionContinuationState, AgentSessionStatus, AppendEventsRequest,
    AwaitEnvironmentReadyActivityRequest, AwaitEnvironmentReadyActivityResult,
    AwaitMaterializationRequest, AwaitMaterializationResult, AwaitOutcome, AwaitPromiseResult,
    CancellingWatchdog, ContextCompactActivityRequest, CreateOrLoadSessionRequest,
    CreateOrLoadSessionResult, JoinedContextPreparationRequest, LLM_PROVIDER_TRANSIENT_ERROR_TYPE,
    LLM_TRANSIENT_FAILURE_DETAILS_VERSION, LlmGenerateActivityRequest, LlmTransientFailureDetails,
    PendingEmission, PendingPromiseCancellation, PendingSourceResolution, PendingToolBatchUnpark,
    PromiseSourcePoll, PutBlobRequest, ReadBlobRequest, ReadBlobResult,
    RuntimeProjectionRefreshActivityRequest, RuntimeProjectionRefreshActivityResult,
    SESSION_DATA_REJECTED_ERROR_TYPE, SessionBootstrapPayloadTooLarge, SubagentChildRef,
    SubagentCloseActivityRequest, SubagentExecutionInput, SubagentExecutionPhase,
    SubagentExecutionResume, SubagentExecutionSnapshot, SubagentLivenessActivityRequest,
    SubagentLivenessActivityResult, SubagentPrepareActivityRequest, SubagentPrepareActivityResult,
    SubagentResolveActivityRequest, SubagentTerminal, SubagentWaitPolicy,
    ToolInvokeBatchActivityRequest, ToolInvokeCallActivityRequest, ToolInvokeCallActivityResult,
    ToolPreparePromiseControlsActivityRequest, WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX,
    WORKFLOW_TOOL_RECIPE_FORMAT_V1, WORKFLOW_TOOL_RECOVERY_QUERY,
    WorkflowToolExecutionCancelRequest, WorkflowToolExecutionCheckRequest, WorkflowToolRecipeV1,
    WorkflowToolRecoveryResult, WorkflowToolReplyValidationRequest,
    WorkflowToolReplyValidationResult, WorkflowToolStartActivityRequest,
    WorkflowToolStartActivityResult, WorkflowToolStartArgs, command_rejection_to_api_error,
    compose_environment_job_workflow_id, compose_workflow_id, split_workflow_id,
    workflow_tool_recipe_fingerprint,
};
pub use workflows::bots;
pub use workflows::channels;
pub use workflows::{
    AgentSessionWorkflow, BotControllerWorkflow, BotTriggerFireWorkflow,
    ChannelConversationWorkflow, CodeExecutionWorkflow, EnvironmentJobWorkflow,
    SubagentExecutionWorkflow, TranscriptionActivityResult, TranscriptionSnapshot,
    TranscriptionWorkflow, TranscriptionWorkflowArgs, emission_signal_input,
    run_terminal_emissions, subagent_child_session_id, subagent_submission_id, transcription_id,
    transcription_workflow_id,
};
