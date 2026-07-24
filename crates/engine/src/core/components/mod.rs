//! CoreAgent components.
//!
//! These modules define the built-in agent's closed command/event/state
//! vocabulary plus the domain-local logic that owns those facts.

pub mod command;
pub mod config;
pub mod context;
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
pub mod workflow_port;

pub use command::*;
pub use config::*;
pub use context::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND, ANTHROPIC_MESSAGES_MCP_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_MCP_TOOL_USE_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND, ContextCompactionStatus,
    ContextCompactionTrigger, ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKind,
    ContextEntrySource, ContextEvent, ContextMessageRole, ContextRemovalReason,
    ContextRewriteReason, ContextSnapshot, ContextState, ENVIRONMENT_ACTIVE_CONTEXT_KEY,
    ENVIRONMENT_CATALOG_CONTEXT_KEY, OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND,
    OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND, OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND,
    OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND, OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND,
    SKILL_ACTIVATION_CONTEXT_KEY_PREFIX, SKILL_ACTIVATION_PROVIDER_KIND_RUN,
    SKILL_ACTIVATION_PROVIDER_KIND_SESSION, SKILL_CATALOG_CONTEXT_KEY, TokenEstimate,
    TokenEstimateQuality, VFS_CATALOG_CONTEXT_KEY, is_run_scoped_skill_activation_entry,
    skill_activation_context_key, validate_external_context_key,
};
pub use error::*;
pub use event::*;
pub use ids::*;
pub use lifecycle::{CoreAgentLifecycleEvent, CoreAgentStatus, LifecycleState};
pub use llm::*;
pub use log::*;
pub use promise::{
    PROMISE_CANCEL_EFFECT_KIND, PROMISE_CREATE_EFFECT_KIND, PROMISE_DETACH_EFFECT_KIND, Promise,
    PromiseComponentState, PromiseEvent, PromiseId, PromiseResolution, PromiseScope, PromiseSource,
    PromiseStatus, promise_cancel_effect, promise_create_effect, promise_detach_effect,
};
pub use run::{
    AcceptedRun, AcceptedRunEvent, ActiveRun, AwaitMode, AwaitOutputRefs, AwaitSpec,
    BufferedMessage, MessageStatus, ParkedAwait, ResumeAwaitCommand, RunEvent, RunFailure,
    RunFailureKind, RunOrigin, RunQueueState, RunRecord, RunRequestCommand, RunRequestSource,
    RunSource, RunSourceContextTrigger, RunStatus, RunTerminalNotifyIntent, SteeringBatch,
    SubmitMessageCommand, WakeReason, message_submission_digest, request_run_submission_digest,
};
pub use state::*;
pub use tooling::{
    ActiveToolBatch, CompletedToolBatch, FunctionToolSpec, ObservedToolCall,
    ProviderNativeToolExecution, ProviderNativeToolSpec, RemoteMcpApprovalPolicy,
    RemoteMcpToolSpec, SecretRef, ToolCallExecutionPolicy, ToolCallResult, ToolCallState,
    ToolCallStatus, ToolChoice, ToolConfigEvent, ToolEvent, ToolExecutionTarget, ToolKind,
    ToolParallelism, ToolPatch, ToolRoutingState, ToolSpec, ToolTargetRequirement, ToolingState,
    UNAVAILABLE_TOOL_RESULT_CONTENT, unavailable_tool_result_ref, validate_tool_map,
};
pub use turn::{
    LlmFinish, LlmGenerationFacts, LlmGenerationStatus, LlmUsage, PlannedRequestState, TurnEvent,
    TurnOutcome, TurnState, TurnStatus,
};
pub use workflow_port::{
    AdmittedManagedSessionWorkflowPorts, MAX_WORKFLOW_PORT_EMISSIONS_PER_READ,
    MAX_WORKFLOW_PORT_EMISSIONS_PER_RUN, ManagedSessionWorkflowPorts, ReadPortEmissionsError,
    WORKFLOW_PORT_EMIT_EFFECT_KIND, WorkflowEndpointRef, WorkflowPortConfigEvent,
    WorkflowPortEvent, WorkflowPortState, WorkflowToolInvocation, WorkflowToolPortBinding,
    WorkflowToolPortDeclaration, WorkflowToolPortDefinition, read_port_emissions,
    workflow_port_emit_effect,
};
