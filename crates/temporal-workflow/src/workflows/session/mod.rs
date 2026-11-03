mod activity_calls;
mod admissions;
mod awaits;
mod bootstrap;
mod clock;
mod code_tools;
mod control;
mod drive;
mod errors;
mod observability;
mod preparation;
mod preparation_candidate;
mod promise_sources;
use preparation::SessionAdmission;
mod session_state;
#[cfg(test)]
mod tests;
mod tool_batches;
mod wait_loop;
mod watchdog;
mod workflow_starts;

use std::time::Duration;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::UNIX_EPOCH,
};

use futures::{FutureExt, pin_mut, select};
use harness::{
    BlobRef, CommandError, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    ContextMessageRole, CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentDriveError,
    CoreAgentEntry, CoreAgentEvent, CoreAgentState, CoreAgentStatus, EmissionEnvelope,
    LlmGenerationRequest, RunEvent, RunStatus, SessionId, SessionPosition, SubmissionId,
    ToolInvocationBatchRequest,
};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    AgentActiveRunSummary, AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind,
    AgentCompletedRunSummary, AgentQueuedRunSummary, AgentSessionArgs,
    AgentSessionContinuationState, AgentSessionStatus, AppendEventsRequest,
    AwaitMaterializationRequest, AwaitOutcome, AwaitPromiseResult, CancellingWatchdog,
    CreateOrLoadSessionRequest, DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
    JoinedContextPreparationRequest, LlmGenerateActivityRequest, PendingEmission,
    PendingPromiseCancellation, PendingSourceResolution, PendingToolBatchResume, PromiseSourcePoll,
    PutBlobRequest, RuntimeProjectionRefreshActivityRequest, ToolInvokeBatchActivityRequest,
    ToolPreparePromiseControlsActivityRequest, WorkflowActivities, activity_options,
    compose_workflow_id, default_instructions, split_workflow_id,
};

use activity_calls::{call_context_compact, call_llm_generate, call_tool_prepare_promise_controls};
use bootstrap::initialize;
use clock::workflow_time_ms;
use drive::{
    CommandAdmissionResult, DriveOutcome, admit_and_append_command, append_command,
    drive_from_state, drive_past_refusals, drive_until_idle, process_pending_tool_batch_resumes,
};
use errors::{fatal, record_admission_failure, record_bootstrap_error, record_warning};
use session_state::flush_pending_emissions;
use wait_loop::{can_continue_as_new, wait_for_workflow_work, workflow_state_should_complete};
use watchdog::{process_cancelling_watchdog, reconcile_cancelling_watchdog};

#[workflow]
pub struct AgentSessionWorkflow {
    universe_id: Option<uuid::Uuid>,
    session_id: Option<SessionId>,
    initialized: bool,
    core_state: CoreAgentState,
    pending_admissions: Vec<SessionAdmission>,
    ready: bool,
    setup_requested: bool,
    setup_error: Option<api::AgentApiError>,
    operation_outcomes: crate::SessionOperationReceipts,
    pending_toolsets: Vec<preparation::PendingToolset>,
    run_preparation: Option<preparation::PendingRunPreparation>,
    pending_tool_batch_resumes: Vec<PendingToolBatchResume>,
    pending_emissions: Vec<PendingEmission>,
    pending_source_resolutions: Vec<PendingSourceResolution>,
    confirmed_workflow_starts: BTreeSet<String>,
    workflow_start_backoffs: BTreeMap<String, (u32, u64)>,
    cancelled_workflow_executions: BTreeSet<String>,
    pending_promise_cancellations: Vec<PendingPromiseCancellation>,
    promise_source_polls: BTreeMap<String, PromiseSourcePoll>,
    cancelling_watchdog: Option<CancellingWatchdog>,
    admission_failures: Vec<AgentAdmissionFailure>,
    execution_has_rollover_checkpoint: bool,
    rollover_delay_logged: bool,
    fatal_error: Option<String>,
    last_warning: Option<String>,
    bootstrap_failed: bool,
    code_tools: code_tools::CodeToolExecutionState,
}

impl Default for AgentSessionWorkflow {
    fn default() -> Self {
        Self {
            universe_id: None,
            session_id: None,
            initialized: false,
            core_state: CoreAgentState::new(),
            pending_admissions: Vec::new(),
            ready: false,
            setup_requested: true,
            setup_error: None,
            operation_outcomes: crate::SessionOperationReceipts::default(),
            pending_toolsets: Vec::new(),
            run_preparation: None,
            pending_tool_batch_resumes: Vec::new(),
            pending_emissions: Vec::new(),
            pending_source_resolutions: Vec::new(),
            confirmed_workflow_starts: BTreeSet::new(),
            workflow_start_backoffs: BTreeMap::new(),
            cancelled_workflow_executions: BTreeSet::new(),
            pending_promise_cancellations: Vec::new(),
            promise_source_polls: BTreeMap::new(),
            cancelling_watchdog: None,
            admission_failures: Vec::new(),
            execution_has_rollover_checkpoint: false,
            rollover_delay_logged: false,
            fatal_error: None,
            last_warning: None,
            bootstrap_failed: false,
            code_tools: Default::default(),
        }
    }
}

#[workflow_methods]
impl AgentSessionWorkflow {
    #[run(name = "AgentSessionWorkflow")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: AgentSessionArgs,
    ) -> WorkflowResult<()> {
        if let Err(error) = initialize(ctx, args.clone()).await {
            record_bootstrap_error(ctx, &error);
            return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!("{error}")).into());
        }

        let preparation_ctx = ctx.clone();
        let preparation = preparation::run_preparation_loop(preparation_ctx).fuse();
        let dispatch_ctx = ctx.clone();
        let code_tool_dispatch = code_tools::run_dispatch_loop(ctx.clone()).fuse();
        let session = async {
            loop {
                preparation::prepare_initial_session(ctx, &args)
                    .await
                    .map_err(|error| fatal(ctx, error, "session_setup"))?;
                if workflow_state_should_complete(ctx) {
                    return Ok(());
                }
                reconcile_cancelling_watchdog(ctx);
                promise_sources::reconcile_polls(ctx);
                wait_for_workflow_work(ctx).await;
                code_tools::process_pending(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "code_tools"))?;
                flush_pending_emissions(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "pending_emission"))?;
                promise_sources::process_pending_source_resolutions(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "promise_source_resolution"))?;
                workflow_starts::process_pending_starts(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "workflow_start"))?;
                promise_sources::flush_pending_promise_cancellations(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "promise_cancellation"))?;
                workflow_starts::process_execution_cancels(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "workflow_execution_cancel"))?;
                awaits::process_satisfied_await(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "await_resolution"))?;
                promise_sources::process_due_promise_deadlines(ctx);
                promise_sources::process_due(ctx)
                    .await
                    .map_err(|error| fatal(ctx, error, "promise_source_poll"))?;
                let resumed = process_pending_tool_batch_resumes(ctx, &args).await;
                let outcome = drive_past_refusals(ctx, &args, resumed)
                    .await
                    .map_err(|error| fatal(ctx, error, "tool_batch_resume"))?;
                if outcome == DriveOutcome::ContinueAsNew {
                    return observability::request_continue_as_new(ctx, &args);
                }
                // After the resumes, so a resume about to land is not pre-empted by
                // the force that exists for the case where none ever does.
                let forced = process_cancelling_watchdog(ctx, &args).await;
                let outcome = drive_past_refusals(ctx, &args, forced)
                    .await
                    .map_err(|error| fatal(ctx, error, "cancellation_watchdog"))?;
                if outcome == DriveOutcome::ContinueAsNew {
                    return observability::request_continue_as_new(ctx, &args);
                }
                let mut admission_drive =
                    drive_from_state(ctx).map_err(|error| fatal(ctx, error, "drive_rehydrate"))?;
                admissions::drain_pending_admissions(ctx, &mut admission_drive)
                    .await
                    .map_err(|error| fatal(ctx, error, "admission"))?;
                if wait_loop::workflow_state_needs_core_drive(ctx) {
                    let mut drive = drive_from_state(ctx)
                        .map_err(|error| fatal(ctx, error, "drive_rehydrate"))?;
                    let driven = drive_until_idle(ctx, &args, &mut drive).await;
                    let outcome = drive_past_refusals(ctx, &args, driven)
                        .await
                        .map_err(|error| fatal(ctx, error, "core_drive"))?;
                    if outcome == DriveOutcome::ContinueAsNew {
                        return observability::request_continue_as_new(ctx, &args);
                    }
                }
                if workflow_state_should_complete(ctx) {
                    return Ok(());
                }
                if can_continue_as_new(ctx, &args) {
                    return observability::request_continue_as_new(ctx, &args);
                }
                observability::observe_rollover_delay(ctx, &args);
            }
        }
        .fuse();
        pin_mut!(session, preparation, code_tool_dispatch);
        futures::select_biased! {
            result = session => result,
            result = code_tool_dispatch => {
                let error = result
                    .err()
                    .unwrap_or_else(|| anyhow::anyhow!("code tool dispatcher stopped"));
                Err(fatal(&dispatch_ctx, error, "code_tool_dispatch").into())
            }
            _ = preparation => unreachable!()
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

    #[signal(name = "prepare_session")]
    pub fn prepare_session(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        request: crate::SessionOperationRequest,
    ) {
        self.pending_admissions
            .push(SessionAdmission::Operation(request));
    }

    #[signal(name = "retry_setup")]
    pub fn retry_setup(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        if !self.ready {
            self.setup_error = None;
            self.setup_requested = true;
        }
    }

    #[query(name = "operation_outcome")]
    pub fn operation_outcome(
        &self,
        _ctx: &WorkflowContextView,
        receipt: crate::SessionOperationReceipt,
    ) -> crate::SessionOperationStatus {
        crate::SessionOperationStatus {
            outcome: self.operation_outcomes.lookup(&receipt),
        }
    }

    /// Fixed inbound funnel for cross-workflow facts. Promise-bearing
    /// emissions become ordinary `ResolvePromise` admissions, preserving the
    /// harness's idempotent first-writer-wins semantics.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        ctx: &mut SyncWorkflowContext<Self>,
        envelope: EmissionEnvelope,
    ) {
        let queued = match split_workflow_id(ctx.workflow_id()) {
            Some((universe_id, _)) => self.queue_emission(universe_id, envelope),
            None => Err(format!(
                "cannot admit emission for malformed session workflow id {}",
                ctx.workflow_id()
            )),
        };
        if let Err(warning) = queued {
            self.record_warning(ctx.is_replaying(), warning);
        }
    }

    #[query(name = "status")]
    pub fn status(&self, _ctx: &WorkflowContextView) -> AgentSessionStatus {
        self.status_snapshot()
    }

    #[update(name = "open_code_tool_scope")]
    pub async fn open_code_tool_scope(
        ctx: &mut WorkflowContext<Self>,
        request: crate::OpenCodeToolScopeRequest,
    ) -> crate::CodeToolScopeResult {
        code_tools::open(ctx, request).await
    }

    #[update(name = "invoke_code_tool")]
    pub async fn invoke_code_tool(
        ctx: &mut WorkflowContext<Self>,
        request: crate::InvokeCodeToolRequest,
    ) -> crate::CodeToolInvocationResult {
        code_tools::invoke(ctx, request).await
    }

    #[update(name = "close_code_tool_scope")]
    pub async fn close_code_tool_scope(
        ctx: &mut WorkflowContext<Self>,
        request: crate::CloseCodeToolScopeRequest,
    ) -> crate::CodeToolScopeResult {
        code_tools::close(ctx, request).await
    }

    #[query(name = "code_tool_scope_report")]
    pub fn code_tool_scope_report(
        &self,
        _ctx: &WorkflowContextView,
        request: crate::CodeToolScopeReportRequest,
    ) -> crate::CodeToolScopeResult {
        code_tools::report(self, &request.execution_id)
    }
}

fn continuation_args(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> AgentSessionArgs {
    let mut next = args.clone();
    next.continuation_state = Some(ctx.state(|state| AgentSessionContinuationState {
        version: AgentSessionContinuationState::VERSION,
        ready: state.ready,
        operation_outcomes: state.operation_outcomes.clone(),
        admission_failures: state.admission_failures.clone(),
    }));
    next
}
