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
    drive_from_state, drive_until_idle, process_pending_tool_batch_resumes,
};
use errors::{record_admission_failure, record_bootstrap_error, record_error};
use session_state::flush_pending_emissions;
use wait_loop::{can_continue_as_new, wait_for_workflow_work, workflow_state_should_complete};
use watchdog::{process_cancelling_watchdog, reconcile_cancelling_watchdog};

#[workflow]
pub struct AgentSessionWorkflow {
    universe_id: Option<uuid::Uuid>,
    session_id: Option<SessionId>,
    initialized: bool,
    core_state: CoreAgentState,
    head: Option<SessionPosition>,
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
    run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    cancelling_watchdog: Option<CancellingWatchdog>,
    admission_failures: Vec<AgentAdmissionFailure>,
    execution_has_rollover_checkpoint: bool,
    rollover_delay_logged: bool,
    last_error: Option<String>,
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
            head: None,
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
            run_submissions: BTreeMap::new(),
            cancelling_watchdog: None,
            admission_failures: Vec::new(),
            execution_has_rollover_checkpoint: false,
            rollover_delay_logged: false,
            last_error: None,
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
        let code_tool_dispatch = code_tools::run_dispatch_loop(ctx.clone()).fuse();
        let session = async {
            loop {
                preparation::prepare_initial_session(ctx, &args)
                    .await
                    .map_err(temporalio_sdk::ApplicationFailure::new)?;
                if workflow_state_should_complete(ctx) {
                    return Ok(());
                }
                reconcile_cancelling_watchdog(ctx);
                promise_sources::reconcile_polls(ctx);
                wait_for_workflow_work(ctx).await;
                code_tools::process_pending(ctx)
                    .await
                    .map_err(temporalio_sdk::ApplicationFailure::new)?;
                if let Err(error) = flush_pending_emissions(ctx).await {
                    record_error(ctx, &error, "pending_emission");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                if let Err(error) = promise_sources::process_pending_source_resolutions(ctx).await {
                    record_error(ctx, &error, "promise_source_resolution");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                if let Err(error) = workflow_starts::process_pending_starts(ctx).await {
                    record_error(ctx, &error, "workflow_start");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                if let Err(error) = promise_sources::flush_pending_promise_cancellations(ctx).await
                {
                    record_error(ctx, &error, "promise_cancellation");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                if let Err(error) = workflow_starts::process_execution_cancels(ctx).await {
                    record_error(ctx, &error, "workflow_execution_cancel");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                match process_cancelling_watchdog(ctx, &args).await {
                    Ok(DriveOutcome::ContinueAsNew) => {
                        return observability::request_continue_as_new(ctx, &args);
                    }
                    Ok(DriveOutcome::Idle | DriveOutcome::YieldForWorkflowWork) => {}
                    Err(error) => {
                        record_error(ctx, &error, "cancellation_watchdog");
                        return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                            "{error}"
                        ))
                        .into());
                    }
                }
                if let Err(error) = awaits::process_satisfied_await(ctx).await {
                    record_error(ctx, &error, "await_resolution");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                promise_sources::process_due_promise_deadlines(ctx);
                if let Err(error) = promise_sources::process_due(ctx).await {
                    record_error(ctx, &error, "promise_source_poll");
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "{error}"
                    ))
                    .into());
                }
                match process_pending_tool_batch_resumes(ctx, &args).await {
                    Ok(DriveOutcome::ContinueAsNew) => {
                        return observability::request_continue_as_new(ctx, &args);
                    }
                    Ok(DriveOutcome::Idle | DriveOutcome::YieldForWorkflowWork) => {}
                    Err(error) => {
                        record_error(ctx, &error, "tool_batch_resume");
                        return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                            "{error}"
                        ))
                        .into());
                    }
                }
                let mut admission_drive =
                    drive_from_state(ctx).map_err(temporalio_sdk::ApplicationFailure::new)?;
                admissions::drain_pending_admissions(ctx, &mut admission_drive)
                    .await
                    .map_err(temporalio_sdk::ApplicationFailure::new)?;
                if wait_loop::workflow_state_needs_core_drive(ctx) {
                    let mut drive = match drive_from_state(ctx) {
                        Ok(drive) => drive,
                        Err(error) => {
                            record_error(ctx, &error, "drive_rehydrate");
                            return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                                "{error}"
                            ))
                            .into());
                        }
                    };
                    match drive_until_idle(ctx, &args, &mut drive).await {
                        Ok(DriveOutcome::ContinueAsNew) => {
                            return observability::request_continue_as_new(ctx, &args);
                        }
                        Ok(DriveOutcome::Idle | DriveOutcome::YieldForWorkflowWork) => {}
                        Err(error) => {
                            record_error(ctx, &error, "core_drive");
                            return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                                "{error}"
                            ))
                            .into());
                        }
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
            result = code_tool_dispatch => Err(temporalio_sdk::ApplicationFailure::new(result.err().unwrap_or_else(|| anyhow::anyhow!("code tool dispatcher stopped"))).into()),
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
        let Some((universe_id, _)) = split_workflow_id(ctx.workflow_id()) else {
            self.last_error = Some(format!(
                "cannot admit emission for malformed session workflow id {}",
                ctx.workflow_id()
            ));
            return;
        };
        self.queue_emission(universe_id, envelope);
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
    next.legacy_max_steps_per_input = None;
    next.continuation_state = Some(ctx.state(|state| {
        let mut continuation = AgentSessionContinuationState::v1(state.admission_failures.clone());
        continuation.ready = state.ready;
        continuation.operation_outcomes = state.operation_outcomes.clone();
        continuation
    }));
    next
}
