use engine::{
    BlobRef, ContextCompactionResult, LlmGenerationResult, PromiseSourceCheckResult,
    ToolBatchOutcome,
};
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::{
    AppendEventsRequest, AwaitEnvironmentReadyActivityRequest, AwaitEnvironmentReadyActivityResult,
    AwaitMaterializationRequest, AwaitMaterializationResult, ContextCompactActivityRequest,
    CreateOrLoadSessionRequest, CreateOrLoadSessionResult, EnvironmentJobCancelActivityRequest,
    EnvironmentJobPollActivityRequest, EnvironmentJobPollActivityResult,
    EnvironmentJobPrepareWorkflowToolRequest, EnvironmentJobStartActivityRequest,
    EnvironmentJobStartActivityResult, JoinedContextPreparationRequest, LlmGenerateActivityRequest,
    PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult, RuntimeProjectionRefreshActivityRequest,
    RuntimeProjectionRefreshActivityResult, SubagentCloseActivityRequest,
    SubagentPrepareActivityRequest, SubagentPrepareActivityResult, SubagentResolveActivityRequest,
    ToolInvokeBatchActivityRequest, ToolInvokeCallActivityRequest, ToolInvokeCallActivityResult,
    ToolPreparePromiseControlsActivityRequest, WorkflowToolExecutionCancelRequest,
    WorkflowToolExecutionCheckRequest, WorkflowToolReplyValidationRequest,
    WorkflowToolReplyValidationResult, WorkflowToolStartActivityRequest,
    WorkflowToolStartActivityResult,
};

pub const ACTIVITY_CREATE_OR_LOAD_SESSION: &str = "WorkflowActivities::create_or_load_session";
pub const ACTIVITY_PUT_BLOB: &str = "WorkflowActivities::put_blob";
pub const ACTIVITY_READ_BLOB: &str = "WorkflowActivities::read_blob";
pub const ACTIVITY_MATERIALIZE_AWAIT_RESULT: &str = "WorkflowActivities::materialize_await_result";
pub const ACTIVITY_PREPARE_JOINED_CONTEXT: &str = "WorkflowActivities::prepare_joined_context";
pub const ACTIVITY_APPEND_EVENTS: &str = "WorkflowActivities::append_events";
pub const ACTIVITY_LLM_GENERATE: &str = "WorkflowActivities::llm_generate";
pub const ACTIVITY_PREPROCESS_RUN_INPUT: &str = "WorkflowActivities::preprocess_run_input";
pub const ACTIVITY_CONTEXT_COMPACT: &str = "WorkflowActivities::context_compact";
pub const ACTIVITY_TOOL_INVOKE_BATCH: &str = "WorkflowActivities::tool_invoke_batch";
pub const ACTIVITY_TOOL_INVOKE_CALL: &str = "WorkflowActivities::tool_invoke_call";
pub const ACTIVITY_TOOL_PREPARE_PROMISE_CONTROLS: &str =
    "WorkflowActivities::tool_prepare_promise_controls";
pub const ACTIVITY_RUNTIME_PROJECTION_REFRESH: &str =
    "WorkflowActivities::runtime_projection_refresh";
pub const ACTIVITY_ENVIRONMENT_JOB_START: &str = "WorkflowActivities::environment_job_start";
pub const ACTIVITY_ENVIRONMENT_JOB_PREPARE_WORKFLOW_TOOL: &str =
    "WorkflowActivities::environment_job_prepare_workflow_tool";
pub const ACTIVITY_ENVIRONMENT_JOB_POLL: &str = "WorkflowActivities::environment_job_poll";
pub const ACTIVITY_ENVIRONMENT_JOB_CANCEL: &str = "WorkflowActivities::environment_job_cancel";
pub const ACTIVITY_VALIDATE_WORKFLOW_TOOL_REPLY: &str =
    "WorkflowActivities::validate_workflow_tool_reply";
pub const ACTIVITY_START_WORKFLOW_TOOL_EXECUTION: &str =
    "WorkflowActivities::start_workflow_tool_execution";
pub const ACTIVITY_CHECK_WORKFLOW_TOOL_EXECUTION: &str =
    "WorkflowActivities::check_workflow_tool_execution";
pub const ACTIVITY_CANCEL_WORKFLOW_TOOL_EXECUTION: &str =
    "WorkflowActivities::cancel_workflow_tool_execution";
pub const ACTIVITY_AWAIT_ENVIRONMENT_READY: &str = "WorkflowActivities::await_environment_ready";
pub const ACTIVITY_SUBAGENT_PREPARE: &str = "WorkflowActivities::subagent_prepare";
pub const ACTIVITY_SUBAGENT_RESOLVE: &str = "WorkflowActivities::subagent_resolve";
pub const ACTIVITY_SUBAGENT_CLOSE: &str = "WorkflowActivities::subagent_close";

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

    /// Load the bounded Promise root refs directly from CAS, write one
    /// canonical aggregate result, and prepare the context entries the
    /// payloads supply beside it; only refs and entries return to history.
    #[activity(name = ACTIVITY_MATERIALIZE_AWAIT_RESULT)]
    pub async fn materialize_await_result(
        _ctx: ActivityContext,
        _request: AwaitMaterializationRequest,
    ) -> Result<AwaitMaterializationResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Prepare, per joined promise, the context entries its payload supplies
    /// beside the call result; the bytes stay in CAS.
    #[activity(name = ACTIVITY_PREPARE_JOINED_CONTEXT)]
    pub async fn prepare_joined_context(
        _ctx: ActivityContext,
        _request: JoinedContextPreparationRequest,
    ) -> Result<Vec<engine::PromiseContextEntries>, ActivityError> {
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

    /// Execute one call of an admitted tool batch. Tool-level failures and
    /// operation deadlines return an ordinary terminal result; only
    /// infrastructure failures fail the activity.
    #[activity(name = ACTIVITY_TOOL_INVOKE_CALL)]
    pub async fn tool_invoke_call(
        _ctx: ActivityContext,
        _request: ToolInvokeCallActivityRequest,
    ) -> Result<ToolInvokeCallActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Wait, with heartbeats, until the session's active environment is
    /// reachable or terminally unusable. Runs outside the per-call tool
    /// activity so tool classes keep their tight deadlines.
    #[activity(name = ACTIVITY_AWAIT_ENVIRONMENT_READY)]
    pub async fn await_environment_ready(
        _ctx: ActivityContext,
        _request: AwaitEnvironmentReadyActivityRequest,
    ) -> Result<AwaitEnvironmentReadyActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_TOOL_PREPARE_PROMISE_CONTROLS)]
    pub async fn tool_prepare_promise_controls(
        _ctx: ActivityContext,
        _request: ToolPreparePromiseControlsActivityRequest,
    ) -> Result<engine::PromiseControlArgumentFacts, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_RUNTIME_PROJECTION_REFRESH)]
    pub async fn runtime_projection_refresh(
        _ctx: ActivityContext,
        _request: RuntimeProjectionRefreshActivityRequest,
    ) -> Result<RuntimeProjectionRefreshActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_START)]
    pub async fn environment_job_start(
        _ctx: ActivityContext,
        _request: EnvironmentJobStartActivityRequest,
    ) -> Result<EnvironmentJobStartActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_PREPARE_WORKFLOW_TOOL)]
    pub async fn environment_job_prepare_workflow_tool(
        _ctx: ActivityContext,
        _request: EnvironmentJobPrepareWorkflowToolRequest,
    ) -> Result<crate::EnvironmentJobWorkflowArgs, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_POLL)]
    pub async fn environment_job_poll(
        _ctx: ActivityContext,
        _request: EnvironmentJobPollActivityRequest,
    ) -> Result<EnvironmentJobPollActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    #[activity(name = ACTIVITY_ENVIRONMENT_JOB_CANCEL)]
    pub async fn environment_job_cancel(
        _ctx: ActivityContext,
        _request: EnvironmentJobCancelActivityRequest,
    ) -> Result<Vec<environment_protocol::data::jobs::JobSummary>, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Bounded CAS load + JSON Schema check of one keyed reply payload
    /// against the binding's immutable reply schema. The deterministic
    /// engine never performs CAS I/O; a receiver cannot bypass reply
    /// validation by returning an arbitrary blob reference.
    #[activity(name = ACTIVITY_VALIDATE_WORKFLOW_TOOL_REPLY)]
    pub async fn validate_workflow_tool_reply(
        _ctx: ActivityContext,
        _request: WorkflowToolReplyValidationRequest,
    ) -> Result<WorkflowToolReplyValidationResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Resolve the trusted start recipe and issue the deterministic
    /// workflow start. `AlreadyStarted` for the exact execution id is
    /// success.
    #[activity(name = ACTIVITY_START_WORKFLOW_TOOL_EXECUTION)]
    pub async fn start_workflow_tool_execution(
        _ctx: ActivityContext,
        _request: WorkflowToolStartActivityRequest,
    ) -> Result<WorkflowToolStartActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Recovery check for one keyed promise of a started execution:
    /// describe the execution and, when it is closed, recover its terminal
    /// result through the fixed recovery query.
    #[activity(name = ACTIVITY_CHECK_WORKFLOW_TOOL_EXECUTION)]
    pub async fn check_workflow_tool_execution(
        _ctx: ActivityContext,
        _request: WorkflowToolExecutionCheckRequest,
    ) -> Result<PromiseSourceCheckResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Best-effort cancellation of the exact system-derived execution.
    #[activity(name = ACTIVITY_CANCEL_WORKFLOW_TOOL_EXECUTION)]
    pub async fn cancel_workflow_tool_execution(
        _ctx: ActivityContext,
        _request: WorkflowToolExecutionCancelRequest,
    ) -> Result<(), ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Validate the pinned sub-agent grant, reserve the root-scoped tree
    /// slot, create the child session from the pinned profile, and start
    /// its run with a notify intent back to the execution.
    #[activity(name = ACTIVITY_SUBAGENT_PREPARE)]
    pub async fn subagent_prepare(
        _ctx: ActivityContext,
        _request: SubagentPrepareActivityRequest,
    ) -> Result<SubagentPrepareActivityResult, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Build the child's result envelope, close the child, and return the
    /// parent's `reply` resolution.
    #[activity(name = ACTIVITY_SUBAGENT_RESOLVE)]
    pub async fn subagent_resolve(
        _ctx: ActivityContext,
        _request: SubagentResolveActivityRequest,
    ) -> Result<engine::PromiseResolution, ActivityError> {
        unimplemented!("workflow activity definition only")
    }

    /// Force-close the child session (cancelled delegation).
    #[activity(name = ACTIVITY_SUBAGENT_CLOSE)]
    pub async fn subagent_close(
        _ctx: ActivityContext,
        _request: SubagentCloseActivityRequest,
    ) -> Result<(), ActivityError> {
        unimplemented!("workflow activity definition only")
    }
}
