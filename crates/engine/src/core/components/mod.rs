//! CoreAgent components.
//!
//! These modules define the built-in agent's closed command/event/state
//! vocabulary plus the domain-local logic that owns those facts.

pub mod approval;
pub mod command;
pub mod config;
pub mod context;
pub mod environment;
pub mod error;
pub mod event;
pub mod ids;
pub mod lifecycle;
pub mod llm;
pub mod log;
pub mod promise;
pub mod run;
pub mod state;
pub mod tooling;
pub mod turn;
pub mod workflow_tool;

pub use approval::*;
pub use command::*;
pub use config::*;
pub use context::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND, ANTHROPIC_MESSAGES_MCP_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_MCP_TOOL_USE_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND, ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
    ContextCompactionStatus, ContextCompactionTrigger, ContextEntry, ContextEntryId,
    ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextEvent, ContextMessageRole,
    ContextRemovalReason, ContextRewriteReason, ContextSnapshot, ContextState,
    OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND, OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND,
    OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND, OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND,
    OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND, OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND,
    OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND, SUPERSEDED_CATALOG_CAP, TokenEstimate,
    TokenEstimateQuality, current_catalog_inputs, current_context_entry,
    is_supersedable_catalog_kind, is_superseded_context_entry, validate_external_context_key,
};
pub use environment::{
    ENVIRONMENT_ACTIVATE_EFFECT_KIND, ENVIRONMENT_DEACTIVATE_EFFECT_KIND, EnvironmentEvent,
    EnvironmentState, environment_activate_effect, environment_deactivate_effect,
};
pub use error::*;
pub use event::*;
pub use ids::*;
pub use lifecycle::{CoreAgentLifecycleEvent, CoreAgentStatus, LifecycleState};
pub use llm::*;
pub use log::*;
pub use promise::{
    PROMISE_CANCEL_EFFECT_KIND, PROMISE_CREATE_EFFECT_KIND, PROMISE_DETACH_EFFECT_KIND,
    PROMISE_ID_PREFIX, Promise, PromiseComponentState, PromiseEvent, PromiseId, PromiseIdAllocator,
    PromiseIdError, PromiseOwnership, PromiseResolution, PromiseScope, PromiseSource,
    PromiseStatus, promise_cancel_effect, promise_create_effect, promise_detach_effect,
};
pub use run::{
    AcceptedRun, AcceptedRunEvent, ActiveRun, AwaitMode, AwaitSpec, JoinedWorkflowCall,
    ParkedToolBatch, PromiseContextEntries, ResumeToolBatchCommand, RunEvent, RunFailure,
    RunFailureKind, RunQueueState, RunRecord, RunRequestCommand, RunRequestSource, RunSource,
    RunStatus, RunTerminalNotifyIntent, SteeringBatch, ToolBatchResumeOutput, ToolBatchSuspension,
    WakeReason, request_run_submission_digest,
};
pub use state::*;
pub use tooling::{
    ActiveToolBatch, BuiltinToolSpec, CANCELLED_TOOL_RESULT_CONTENT, CompletedToolBatch,
    FunctionToolSpec, ObservedToolCall, ProviderNativeToolExecution, ProviderNativeToolSpec,
    RemoteMcpApprovalPolicy, RemoteMcpExecution, RemoteMcpExposure, RemoteMcpToolSpec, SecretRef,
    TOOL_RUNTIME_BOUNDARY_FAILURE_CONTENT, ToolCallExecutionPolicy, ToolCallResult, ToolCallState,
    ToolCallStatus, ToolChoice, ToolConfigEvent, ToolEvent, ToolExecutionClass, ToolExecutionSpec,
    ToolKind, ToolParallelism, ToolPatch, ToolSpec, ToolingState, UNAVAILABLE_TOOL_RESULT_CONTENT,
    cancelled_tool_result_ref, remote_mcp_call_runtime, tool_runtime_boundary_failure_ref,
    unavailable_tool_result_ref, validate_tool_map,
};
pub use turn::{
    LlmFinish, LlmGenerationFacts, LlmGenerationStatus, LlmUsage, PlannedRequestState, TurnEvent,
    TurnOutcome, TurnState, TurnStatus,
};
pub use workflow_tool::{
    AdmittedManagedSessionWorkflowTools, BoundWorkflowToolDispatch, MAX_COMPLETION_PROMISES,
    MAX_WORKFLOW_TOOL_EMISSIONS_PER_READ, MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN,
    ManagedSessionWorkflowTools, REPLY_COMPLETION_KEY, ReadToolEmissionsError,
    WORKFLOW_TOOL_EMIT_EFFECT_KIND, WORKFLOW_TOOL_EXECUTION_KIND, WorkflowEndpointRef,
    WorkflowStartRef, WorkflowToolBinding, WorkflowToolCompletion, WorkflowToolCompletionKeySource,
    WorkflowToolConfigEvent, WorkflowToolDeclaration, WorkflowToolDefinition, WorkflowToolEvent,
    WorkflowToolInvocation, WorkflowToolState, WorkflowToolTarget, completion_promise_source,
    read_tool_emissions, validate_completion_key, with_completion_deadline,
    workflow_tool_emit_effect, workflow_tool_execution_id,
};
