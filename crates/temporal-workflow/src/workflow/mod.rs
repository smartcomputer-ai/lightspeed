mod activity_calls;
mod admissions;
mod bootstrap;
mod clock;
mod drive;
mod errors;
mod fleet_waits;
mod job_waits;
mod session_state;
#[cfg(test)]
mod tests;
mod wait_loop;

use std::time::Duration;
use std::{collections::BTreeMap, time::UNIX_EPOCH};

use engine::{
    ApplyEvent, BlobRef, CommandCodec, CommandError, ContextEntryInput, ContextEntryKey,
    ContextEntryKind, ContextMessageRole, CoreAgentAction, CoreAgentCodec, CoreAgentCommand,
    CoreAgentDrive, CoreAgentDriveError, CoreAgentEntry, CoreAgentEventKind, CoreAgentState,
    CoreAgentStatus, CoreApplyEvent, ENVIRONMENT_ACTIVE_CONTEXT_KEY,
    ENVIRONMENT_CATALOG_CONTEXT_KEY, LlmGenerationRequest, RunEvent, RunStatus,
    SKILL_CATALOG_CONTEXT_KEY, SessionId, SessionPosition, SubmissionId, ToolCallStatus, ToolEvent,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationResult,
    VFS_CATALOG_CONTEXT_KEY,
};
use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    ActiveEnvironmentJobWait, ActiveWaitRecord, ActiveWaitSubscription, AgentActiveRunSummary,
    AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind, AgentCompletedRunSummary,
    AgentQueuedRunSummary, AgentSessionArgs, AgentSessionStatus, AgentWaitDirective,
    AgentWaitHandleResult, AgentWaitHandleStatus, AgentWaitMode, AgentWaitOutcome, AgentWaitOutput,
    AgentWaitRunResult, AppendEventsRequest, CheckEnvironmentJobWaitActivityRequest,
    CheckEnvironmentJobWaitActivityResult, CreateOrLoadSessionRequest,
    DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD, EnvironmentJobChanged,
    FLEET_AGENT_WAIT_DIRECTIVE_KIND, LlmGenerateActivityRequest, PendingRunTerminalNotification,
    PendingToolBatchResume, PreprocessRunInputActivityRequest, PreprocessRunInputFailure,
    PreprocessRunInputFailureKind, PreprocessRunInputOutcome, PutBlobRequest, RunSubscription,
    RunTerminalNotification, SkillCatalogRefreshActivityRequest, ToolInvokeBatchActivityRequest,
    WorkflowActivities, activity_options, default_instructions,
};

use activity_calls::{
    call_context_compact, call_llm_generate, call_tool_invoke_batch, check_environment_job_wait,
};
use admissions::process_admissions;
use bootstrap::initialize;
use clock::workflow_time_ms;
use drive::{
    CommandAdmissionResult, admit_and_append_command, append_command, drive_from_state,
    drive_until_idle, process_pending_tool_batch_resumes,
};
use errors::{record_admission_failure, record_bootstrap_error, record_error};
use fleet_waits::{
    active_wait_nontimer_resolution, install_deferred_wait, mark_wait_terminal_arrival,
    process_satisfied_active_waits, wait_directive_for_event,
};
use session_state::flush_pending_terminal_notifications;
use wait_loop::{
    can_continue_as_new_at_idle, wait_for_workflow_work, workflow_state_should_complete,
};

const DEFAULT_MAX_STEPS_PER_INPUT: usize = 256;

#[workflow(name = "AgentSessionWorkflow")]
pub struct AgentSessionWorkflow {
    session_id: Option<SessionId>,
    initialized: bool,
    core_state: CoreAgentState,
    head: Option<SessionPosition>,
    pending_admissions: Vec<AgentAdmission>,
    pending_tool_batch_resumes: Vec<PendingToolBatchResume>,
    pending_terminal_notifications: Vec<PendingRunTerminalNotification>,
    active_waits: BTreeMap<u64, ActiveWaitRecord>,
    active_environment_job_waits: BTreeMap<u64, ActiveEnvironmentJobWait>,
    run_subscriptions: BTreeMap<String, RunSubscription>,
    run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    admission_failures: Vec<AgentAdmissionFailure>,
    last_error: Option<String>,
    bootstrap_failed: bool,
}

impl Default for AgentSessionWorkflow {
    fn default() -> Self {
        Self {
            session_id: None,
            initialized: false,
            core_state: CoreAgentState::new(),
            head: None,
            pending_admissions: Vec::new(),
            pending_tool_batch_resumes: Vec::new(),
            pending_terminal_notifications: Vec::new(),
            active_waits: BTreeMap::new(),
            active_environment_job_waits: BTreeMap::new(),
            run_subscriptions: BTreeMap::new(),
            run_submissions: BTreeMap::new(),
            admission_failures: Vec::new(),
            last_error: None,
            bootstrap_failed: false,
        }
    }
}

#[workflow_methods]
impl AgentSessionWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: AgentSessionArgs,
    ) -> WorkflowResult<()> {
        if let Err(error) = initialize(ctx, args.clone()).await {
            record_bootstrap_error(ctx, &error);
            return Err(anyhow::anyhow!("{error}").into());
        }

        loop {
            if workflow_state_should_complete(ctx) {
                return Ok(());
            }
            wait_for_workflow_work(ctx).await;
            if let Err(error) = flush_pending_terminal_notifications(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = process_satisfied_active_waits(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = job_waits::process_due(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = process_pending_tool_batch_resumes(ctx, &args).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            let admissions = ctx.state_mut(|state| std::mem::take(&mut state.pending_admissions));
            if !admissions.is_empty()
                && let Err(error) = process_admissions(ctx, &args, admissions).await
            {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if workflow_state_should_complete(ctx) {
                return Ok(());
            }
            if can_continue_as_new_at_idle(ctx, &args) {
                ctx.continue_as_new(&args, ContinueAsNewOptions::default())?;
            }
        }
    }

    #[signal(name = "submit_admission")]
    pub fn submit_admission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        admission: AgentAdmission,
    ) {
        self.queue_admission(admission);
    }

    #[signal(name = "subscribe_run")]
    pub fn subscribe_run(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        subscription: RunSubscription,
    ) {
        self.subscribe_to_run(subscription);
    }

    #[signal(name = "unsubscribe_run")]
    pub fn unsubscribe_run(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        subscription_id: String,
    ) {
        self.unsubscribe_from_run(&subscription_id);
    }

    #[signal(name = "run_terminal")]
    pub fn run_terminal(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        notification: RunTerminalNotification,
    ) {
        self.record_run_terminal(notification);
    }

    #[signal(name = "environment_job_changed")]
    pub fn environment_job_changed(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        changed: EnvironmentJobChanged,
    ) {
        self.record_environment_job_changed(changed);
    }

    #[query(name = "status")]
    pub fn status(&self, _ctx: &WorkflowContextView) -> AgentSessionStatus {
        self.status_snapshot()
    }
}
