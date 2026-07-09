use super::*;

pub(super) async fn wait_for_workflow_work(ctx: &mut WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    if workflow_has_immediate_work(ctx, now) {
        return;
    }

    let Some(deadline_ms) = nearest_workflow_wake_ms(ctx) else {
        ctx.wait_condition(|state| workflow_state_has_immediate_work(state))
            .await;
        return;
    };
    if deadline_ms <= now {
        return;
    }

    let duration = Duration::from_millis(deadline_ms - now);
    let wake = {
        let wait = ctx.wait_condition(|state| workflow_state_has_immediate_work(state));
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
    !state.pending_admissions.is_empty()
        || !state.pending_tool_batch_resumes.is_empty()
        || !state.pending_promise_notifications.is_empty()
        || !state.pending_promise_cancellations.is_empty()
        || awaits::has_satisfied_await(state)
        || promise_sources::has_immediate_work(state)
}

fn nearest_workflow_wake_ms(ctx: &WorkflowContext<AgentSessionWorkflow>) -> Option<u64> {
    ctx.state(nearest_workflow_wake_ms_for_state)
}

fn nearest_workflow_wake_ms_for_state(state: &AgentSessionWorkflow) -> Option<u64> {
    let await_deadline = awaits::nearest_await_wake_ms(state);
    let promise_source_deadline = promise_sources::nearest_wake_ms(state);
    let watchdog_deadline = watchdog::cancelling_watchdog_wake_ms(state);
    [await_deadline, promise_source_deadline, watchdog_deadline]
        .into_iter()
        .flatten()
        .min()
}

pub(super) fn can_continue_as_new_at_idle(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> bool {
    !workflow_state_should_complete(ctx)
        && ctx.state(workflow_state_allows_continue_as_new)
        && should_continue_as_new(
            ctx.continue_as_new_suggested(),
            ctx.history_length(),
            args.continue_as_new_history_threshold,
        )
}

/// Continue-as-new needs quiescence of in-flight transport only: pending
/// admissions, unresumed batches, and the outbound notify flush queue.
/// Parked awaits and promise-source polls are log-derived and never block
/// CAN.
pub(super) fn workflow_state_allows_continue_as_new(state: &AgentSessionWorkflow) -> bool {
    state.pending_admissions.is_empty()
        && state.pending_tool_batch_resumes.is_empty()
        && state.pending_promise_notifications.is_empty()
        && state.pending_promise_cancellations.is_empty()
}

pub(super) fn workflow_state_should_complete(ctx: &WorkflowContext<AgentSessionWorkflow>) -> bool {
    ctx.state(workflow_state_is_closed_and_quiescent)
}

pub(super) fn workflow_state_is_closed_and_quiescent(state: &AgentSessionWorkflow) -> bool {
    state.initialized
        && state.core_state.lifecycle.status == CoreAgentStatus::Closed
        && state.pending_admissions.is_empty()
        && state.pending_tool_batch_resumes.is_empty()
        && state.pending_promise_notifications.is_empty()
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
