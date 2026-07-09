mod activity_calls;
mod admissions;
mod awaits;
mod bootstrap;
mod clock;
mod drive;
mod errors;
mod promise_sources;
mod session_state;
#[cfg(test)]
mod tests;
mod wait_loop;
mod watchdog;

use std::time::Duration;
use std::{collections::BTreeMap, time::UNIX_EPOCH};

use engine::{
    BlobRef, CommandError, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    ContextMessageRole, CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentDriveError,
    CoreAgentEntry, CoreAgentEvent, CoreAgentState, CoreAgentStatus,
    ENVIRONMENT_ACTIVE_CONTEXT_KEY, ENVIRONMENT_CATALOG_CONTEXT_KEY, LlmGenerationRequest,
    RunConfig, RunEvent, RunStatus, SKILL_CATALOG_CONTEXT_KEY, SessionId, SessionPosition,
    SubmissionId, ToolInvocationBatchRequest, VFS_CATALOG_CONTEXT_KEY,
};
use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    AgentActiveRunSummary, AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind,
    AgentCompletedRunSummary, AgentMessageSubmissionConsumptionSummary, AgentQueuedRunSummary,
    AgentSessionArgs, AgentSessionStatus, AppendEventsRequest, AwaitOutcome, AwaitOutput,
    AwaitPromiseResult, CancellingWatchdog, CreateOrLoadSessionRequest,
    DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD, EnvironmentJobChanged, LlmGenerateActivityRequest,
    PendingPromiseCancellation, PendingPromiseNotification, PendingToolBatchResume,
    PreprocessRunInputActivityRequest, PreprocessRunInputFailure, PreprocessRunInputFailureKind,
    PreprocessRunInputOutcome, PromiseResolutionSignal, PromiseSourcePoll, PutBlobRequest,
    RuntimeProjectionRefreshActivityRequest, ToolInvokeBatchActivityRequest, WorkflowActivities,
    activity_options, compose_workflow_id, default_instructions, split_workflow_id,
};

use activity_calls::{call_context_compact, call_llm_generate, call_tool_invoke_batch};
use admissions::process_admissions;
use bootstrap::initialize;
use clock::workflow_time_ms;
use drive::{
    CommandAdmissionResult, admit_and_append_command, append_command, drive_from_state,
    drive_until_idle, process_pending_tool_batch_resumes,
};
use errors::{record_admission_failure, record_bootstrap_error, record_error};
use session_state::flush_pending_promise_notifications;
use wait_loop::{
    can_continue_as_new_at_idle, wait_for_workflow_work, workflow_state_should_complete,
};
use watchdog::{process_cancelling_watchdog, reconcile_cancelling_watchdog};

const DEFAULT_MAX_STEPS_PER_INPUT: usize = 256;

#[workflow(name = "AgentSessionWorkflow")]
pub struct AgentSessionWorkflow {
    session_id: Option<SessionId>,
    initialized: bool,
    core_state: CoreAgentState,
    head: Option<SessionPosition>,
    pending_admissions: Vec<AgentAdmission>,
    pending_tool_batch_resumes: Vec<PendingToolBatchResume>,
    pending_promise_notifications: Vec<PendingPromiseNotification>,
    pending_promise_cancellations: Vec<PendingPromiseCancellation>,
    promise_source_polls: BTreeMap<String, PromiseSourcePoll>,
    run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    cancelling_watchdog: Option<CancellingWatchdog>,
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
            pending_promise_notifications: Vec::new(),
            pending_promise_cancellations: Vec::new(),
            promise_source_polls: BTreeMap::new(),
            run_submissions: BTreeMap::new(),
            cancelling_watchdog: None,
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
            reconcile_cancelling_watchdog(ctx);
            promise_sources::reconcile_polls(ctx);
            wait_for_workflow_work(ctx).await;
            if let Err(error) = flush_pending_promise_notifications(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = promise_sources::flush_pending_promise_cancellations(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = process_cancelling_watchdog(ctx, &args).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = awaits::process_satisfied_await(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = promise_sources::process_due(ctx).await {
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

    /// Queues a batch of admissions atomically: entries in one signal are
    /// processed contiguously, so a multi-entry `session/context/append` cannot
    /// interleave with admissions from concurrent requests.
    #[signal(name = "submit_admissions")]
    pub fn submit_admissions(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        admissions: Vec<AgentAdmission>,
    ) {
        for admission in admissions {
            self.queue_admission(admission);
        }
    }

    /// Push delivery from a session this session holds a promise on: the
    /// run behind the promise reached a terminal state. Queued as a
    /// `ResolvePromise` admission (idempotent, first-writer-wins).
    #[signal(name = "resolve_promise")]
    pub fn resolve_promise(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        signal: PromiseResolutionSignal,
    ) {
        self.queue_promise_resolution(signal);
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
