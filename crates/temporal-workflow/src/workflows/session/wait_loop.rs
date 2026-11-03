use super::*;
use crate::workflows::WorkflowContextExt as _;

pub(super) async fn wait_for_workflow_work(ctx: &mut WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    if workflow_has_immediate_work(ctx, now) {
        return;
    }

    let Some(deadline_ms) = nearest_workflow_wake_ms(ctx) else {
        let wait = ctx.wait_for_state(workflow_state_has_immediate_work);
        pin_mut!(wait);
        wait.await;
        return;
    };
    if deadline_ms <= now {
        return;
    }

    let duration = Duration::from_millis(deadline_ms - now);
    let wait = ctx.wait_for_state(workflow_state_has_immediate_work);
    let timer = ctx.timer_with_manual_cancellation(duration).fuse();
    pin_mut!(wait, timer);
    select_biased! {
        _ = wait => {}
        _ = timer => {}
    }
}

fn workflow_has_immediate_work(ctx: &WorkflowContext<AgentSessionWorkflow>, now: u64) -> bool {
    ctx.state(|state| {
        workflow_state_has_immediate_work(state)
            || nearest_workflow_wake_ms_for_state(state).is_some_and(|deadline| deadline <= now)
    })
}

pub(super) fn workflow_state_has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    // A final Update handler may finish after the last durable append. Wake
    // the owner so it can recheck all_handlers_finished and complete, even
    // when no further event, promise, or timer can drive the loop.
    workflow_state_is_closed_and_quiescent(state)
        || (!state.ready && state.setup_requested)
        || admissions::has_admissible_admissions(state)
        || !state.pending_tool_batch_resumes.is_empty()
        || code_tools::has_immediate_work(state)
        || session_state::has_due_emissions(state, 0)
        || !state.pending_source_resolutions.is_empty()
        || !state.pending_promise_cancellations.is_empty()
        || !state.pending_execution_cancels.is_empty()
        || awaits::has_satisfied_await(state)
        || promise_sources::has_immediate_work(state)
        || workflow_starts::has_immediate_work(state)
        || workflow_state_needs_core_drive_for_state(state)
}

/// Whether the outer loop would consume a rollover blocker on its next pass, with an
/// await in it. An armed watchdog, unfinished update handlers, code tools in flight,
/// backoffs not yet due and admissions held behind a turn or a compaction do not
/// count: those clear only through the drive or the clock.
pub(super) fn outer_loop_can_clear_now(state: &AgentSessionWorkflow, now_ms: u64) -> bool {
    admissions::has_admissible_admissions(state)
        || !state.pending_tool_batch_resumes.is_empty()
        || session_state::has_due_emissions(state, now_ms)
        || !state.pending_source_resolutions.is_empty()
        || !state.pending_promise_cancellations.is_empty()
        || code_tools::has_immediate_work(state)
        || workflow_starts::has_due_start(state, now_ms)
}

/// Drop rollover blockers that have already ended. The outer loop does this on its
/// passes, but a drive can go thousands of turns without returning there, and a
/// watchdog left by a finished cancel or a backoff whose promise has ended would hold
/// a due rollover shut the whole time, with nothing to yield for.
pub(super) fn forget_ended_rollover_blockers(state: &mut AgentSessionWorkflow, now_ms: u64) {
    watchdog::reconcile_cancelling_watchdog_for_state(state, now_ms);
    workflow_starts::forget_stale_backoffs(state);
}

/// Outbound work only the outer loop services, ready now. One service pass leaves
/// this false: a delivered or started item is gone, a failed one waits out a backoff
/// that lies ahead of the clock, and a due deadline has become a resolution.
pub(super) fn transport_work_due(state: &AgentSessionWorkflow, now_ms: u64) -> bool {
    session_state::has_due_emissions(state, now_ms)
        || !state.pending_source_resolutions.is_empty()
        || !state.pending_promise_cancellations.is_empty()
        || workflow_starts::has_due_start(state, now_ms)
        || promise_sources::has_due_promise_deadline(state, now_ms)
        || !state.pending_execution_cancels.is_empty()
}

/// Passes of the outer loop allowed in one activation with nothing awaited.
const STALLED_PASS_LIMIT: u32 = 32;

/// Catches a session loop that stopped awaiting. A workflow task's clock and history
/// length move only between activations, so a pass that repeats both awaited nothing.
/// One such loop would hold every workflow on this worker's thread.
#[derive(Default)]
pub(super) struct StallGuard {
    seen: (u32, u64),
    passes: u32,
}

impl StallGuard {
    /// Note the start of a pass, and say whether the loop should wait out a timer.
    pub(super) fn stalled(&mut self, history_length: u32, now_ms: u64) -> bool {
        if self.seen != (history_length, now_ms) {
            self.seen = (history_length, now_ms);
            self.passes = 0;
        }
        self.passes += 1;
        if self.passes < STALLED_PASS_LIMIT {
            return false;
        }
        self.passes = 0;
        true
    }
}

pub(super) fn workflow_state_needs_core_drive(ctx: &WorkflowContext<AgentSessionWorkflow>) -> bool {
    ctx.state(workflow_state_needs_core_drive_for_state)
}

pub(super) fn workflow_state_needs_core_drive_for_state(state: &AgentSessionWorkflow) -> bool {
    state.ready
        && ((!state.pending_toolsets.is_empty() && !harness::tools_frozen(&state.core_state))
            // Queued runs cannot start behind an active run. Treating them
            // as immediate work while it is parked spins without yielding,
            // preventing Temporal timers and incoming controls from advancing.
            || (state.core_state.runs.active.is_none()
                && !state.core_state.runs.queued.is_empty())
            || state.core_state.context.compaction.is_pending()
            || state.core_state.runs.active.as_ref().is_some_and(|run| {
                awaits::parked_tool_batch(&state.core_state).is_none()
                    && !(run.status == harness::RunStatus::Parked
                        && run.pending_approvals().next().is_some())
            }))
}

fn nearest_workflow_wake_ms(ctx: &WorkflowContext<AgentSessionWorkflow>) -> Option<u64> {
    ctx.state(nearest_workflow_wake_ms_for_state)
}

fn nearest_workflow_wake_ms_for_state(state: &AgentSessionWorkflow) -> Option<u64> {
    let await_deadline = awaits::nearest_await_wake_ms(state);
    let promise_source_deadline = promise_sources::nearest_wake_ms(state);
    let watchdog_deadline = watchdog::cancelling_watchdog_wake_ms(state);
    let emission_retry_deadline = session_state::nearest_emission_retry_ms(state);
    let workflow_start_deadline = workflow_starts::nearest_wake_ms(state);
    let promise_hard_deadline = promise_sources::nearest_promise_deadline_ms(state);
    let code_tool_deadline = code_tools::nearest_wake_ms(state);
    [
        await_deadline,
        promise_source_deadline,
        watchdog_deadline,
        emission_retry_deadline,
        workflow_start_deadline,
        promise_hard_deadline,
        code_tool_deadline,
    ]
    .into_iter()
    .flatten()
    .min()
}

pub(super) fn can_continue_as_new(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> bool {
    !workflow_state_should_complete(ctx)
        && ctx.all_handlers_finished()
        && ctx.state(workflow_state_allows_continue_as_new)
        && history_rollover_due(ctx, args)
}

pub(super) fn history_rollover_due(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> bool {
    ctx.state(|state| state.execution_has_rollover_checkpoint)
        && should_continue_as_new(
            ctx.continue_as_new_suggested(),
            ctx.history_length(),
            args.continue_as_new_history_threshold,
        )
}

/// Continue-as-new needs quiescence of in-flight transport plus the two local
/// clocks whose original deadlines are not log-derived. Parked awaits and
/// promise-source polls are reconstructed from durable promise/run state.
/// Confirmed starts may be retried safely because their workflow execution
/// identities are stable, and queued execution cancels travel with the continuation.
pub(super) fn workflow_state_allows_continue_as_new(state: &AgentSessionWorkflow) -> bool {
    state.ready
        && code_tools::is_quiescent(state)
        && state.run_preparation.is_none()
        && state.pending_toolsets.is_empty()
        && state.pending_admissions.is_empty()
        && state.pending_tool_batch_resumes.is_empty()
        && state.pending_emissions.is_empty()
        && state.pending_source_resolutions.is_empty()
        && state.pending_promise_cancellations.is_empty()
        && state.workflow_start_backoffs.is_empty()
        && state.cancelling_watchdog.is_none()
}

pub(super) fn workflow_state_should_complete(ctx: &WorkflowContext<AgentSessionWorkflow>) -> bool {
    ctx.all_handlers_finished() && ctx.state(workflow_state_is_closed_and_quiescent)
}

pub(super) fn workflow_state_is_closed_and_quiescent(state: &AgentSessionWorkflow) -> bool {
    state.initialized
        && code_tools::is_quiescent(state)
        && state.core_state.lifecycle.status == CoreAgentStatus::Closed
        && state.run_preparation.is_none()
        && state.pending_toolsets.is_empty()
        && state.pending_admissions.is_empty()
        && state.pending_tool_batch_resumes.is_empty()
        && state.pending_emissions.is_empty()
        && state.pending_source_resolutions.is_empty()
        && state.pending_promise_cancellations.is_empty()
        && state.pending_execution_cancels.is_empty()
        && state.core_state.runs.active.is_none()
        && state.core_state.runs.queued.is_empty()
}

pub(super) fn should_continue_as_new(
    suggested: bool,
    history_length: u32,
    history_threshold: Option<u32>,
) -> bool {
    suggested
        || history_length >= history_threshold.unwrap_or(DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD)
}
