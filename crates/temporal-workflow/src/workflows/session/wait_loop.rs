use super::*;

pub(super) const ACTIVE_RUN_ROLLOVER_PATCH: &str = "p105_active_run_rollover_v1";

pub(super) async fn wait_for_workflow_work(ctx: &mut WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    if workflow_has_immediate_work(ctx, now) {
        return;
    }

    let Some(deadline_ms) = nearest_workflow_wake_ms(ctx) else {
        let wait = ctx.wait_condition(workflow_state_has_immediate_work);
        pin_mut!(wait);
        wait.await;
        return;
    };
    if deadline_ms <= now {
        return;
    }

    let duration = Duration::from_millis(deadline_ms - now);
    let wake = {
        let wait = ctx.wait_condition(workflow_state_has_immediate_work);
        let timer = ctx.timer(duration).fuse();
        pin_mut!(wait, timer);
        select! {
            _ = wait => WorkflowWake::State,
            _ = timer => WorkflowWake::Timer,
        }
    };
    let _ = wake;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkflowWake {
    State,
    Timer,
}

fn workflow_has_immediate_work(ctx: &WorkflowContext<AgentSessionWorkflow>, now: u64) -> bool {
    ctx.state(|state| {
        workflow_state_has_immediate_work(state)
            || nearest_workflow_wake_ms_for_state(state).is_some_and(|deadline| deadline <= now)
    })
}

pub(super) fn workflow_state_has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    (!state.ready && state.setup_requested)
        || admissions::has_admissible_admissions(state)
        || !state.pending_tool_batch_resumes.is_empty()
        || session_state::has_due_emissions(state)
        || !state.pending_source_resolutions.is_empty()
        || !state.pending_promise_cancellations.is_empty()
        || awaits::has_satisfied_await(state)
        || promise_sources::has_immediate_work(state)
        || workflow_starts::has_immediate_work(state)
        || workflow_state_needs_core_drive_for_state(state)
}

pub(super) fn workflow_state_needs_core_drive(ctx: &WorkflowContext<AgentSessionWorkflow>) -> bool {
    ctx.state(workflow_state_needs_core_drive_for_state)
}

pub(super) fn workflow_state_needs_core_drive_for_state(state: &AgentSessionWorkflow) -> bool {
    state.ready
        && (!state.pending_toolsets.is_empty()
            || !state.core_state.runs.queued.is_empty()
            || state.core_state.context.pending_compaction
            || state.core_state.runs.active.as_ref().is_some_and(|run| {
                awaits::parked_tool_batch(&state.core_state).is_none()
                    && !(run.status == engine::RunStatus::Parked
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
    [
        await_deadline,
        promise_source_deadline,
        watchdog_deadline,
        emission_retry_deadline,
        workflow_start_deadline,
        promise_hard_deadline,
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
        && ctx.state(workflow_state_allows_continue_as_new)
        && history_rollover_due(ctx, args)
}

pub(super) fn history_rollover_due(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> bool {
    let rollover_enabled = ctx.patched(ACTIVE_RUN_ROLLOVER_PATCH);
    (!rollover_enabled || ctx.state(|state| state.execution_has_rollover_checkpoint))
        && should_continue_as_new(
            ctx.continue_as_new_suggested(),
            ctx.history_length(),
            args.continue_as_new_history_threshold,
        )
}

/// Continue-as-new needs quiescence of in-flight transport plus the two local
/// clocks whose original deadlines are not log-derived. Parked awaits and
/// promise-source polls are reconstructed from durable promise/run state.
/// Confirmed starts and issued execution cancellations may be retried safely
/// because their workflow execution identities are stable.
pub(super) fn workflow_state_allows_continue_as_new(state: &AgentSessionWorkflow) -> bool {
    state.ready
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
    ctx.state(workflow_state_is_closed_and_quiescent)
}

pub(super) fn workflow_state_is_closed_and_quiescent(state: &AgentSessionWorkflow) -> bool {
    state.initialized
        && state.core_state.lifecycle.status == CoreAgentStatus::Closed
        && state.run_preparation.is_none()
        && state.pending_toolsets.is_empty()
        && state.pending_admissions.is_empty()
        && state.pending_tool_batch_resumes.is_empty()
        && state.pending_emissions.is_empty()
        && state.pending_source_resolutions.is_empty()
        && state.pending_promise_cancellations.is_empty()
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
