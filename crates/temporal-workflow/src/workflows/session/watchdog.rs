use super::*;

/// How long a run may sit in cancellation or its grace turn before the
/// workflow forces it to `cancelled`. A missed edge in the cancellation state
/// machine must degrade to a forced transition, never a permanent wedge.
pub(super) const CANCELLING_WATCHDOG_MS: u64 = 60_000;

/// Keep the watchdog record in sync with core state: arm it when the active
/// run enters cancellation, hold it through the grace turn, and drop it when
/// that run leaves cancellation (or a different run becomes active).
pub(super) fn reconcile_cancelling_watchdog(ctx: &WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    ctx.state_mut(|state| {
        state.cancelling_watchdog = next_cancelling_watchdog(
            cancelling_run_id(&state.core_state),
            state.cancelling_watchdog,
            now,
        );
    });
}

pub(super) fn cancelling_run_id(core_state: &CoreAgentState) -> Option<u64> {
    core_state
        .runs
        .active
        .as_ref()
        .filter(|run| {
            matches!(
                run.status,
                RunStatus::Cancelling | RunStatus::CancellingGrace
            )
        })
        .map(|run| run.run_id.as_u64())
}

pub(super) fn next_cancelling_watchdog(
    cancelling_run_id: Option<u64>,
    previous: Option<CancellingWatchdog>,
    now_ms: u64,
) -> Option<CancellingWatchdog> {
    match (cancelling_run_id, previous) {
        (Some(run_id), Some(watchdog)) if watchdog.run_id == run_id => Some(watchdog),
        (Some(run_id), _) => Some(CancellingWatchdog {
            run_id,
            since_ms: now_ms,
        }),
        (None, _) => None,
    }
}

pub(super) fn cancelling_watchdog_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .cancelling_watchdog
        .map(|watchdog| watchdog.since_ms.saturating_add(CANCELLING_WATCHDOG_MS))
}

/// Force-cancel the active run if it has been cancelling past the watchdog
/// deadline. `ForceCancelRun` admission is an idempotent no-op when the run
/// already reached a terminal state, so racing a late normal cancellation is
/// harmless.
pub(super) async fn process_cancelling_watchdog(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let expired_run_id = ctx.state(|state| {
        state.cancelling_watchdog.and_then(|watchdog| {
            let expired = now >= watchdog.since_ms.saturating_add(CANCELLING_WATCHDOG_MS);
            let still_cancelling = state.core_state.runs.active.as_ref().is_some_and(|run| {
                run.run_id.as_u64() == watchdog.run_id
                    && matches!(
                        run.status,
                        RunStatus::Cancelling | RunStatus::CancellingGrace
                    )
            });
            (expired && still_cancelling).then_some(watchdog.run_id)
        })
    });
    let Some(run_id) = expired_run_id else {
        return Ok(());
    };
    let mut drive = drive_from_state(ctx)?;
    let command = CoreAgentCommand::ForceCancelRun {
        run_id: engine::RunId::new(run_id),
    };
    match admit_and_append_command(ctx, &mut drive, command, None).await? {
        CommandAdmissionResult::Accepted => {}
        CommandAdmissionResult::Rejected(failure) => {
            // Nothing left to force (session closed underneath us); record
            // and move on rather than failing the session loop.
            record_admission_failure(ctx, failure);
            return Ok(());
        }
    }
    ctx.state_mut(|state| state.cancelling_watchdog = None);
    drive_until_idle(ctx, args, &mut drive).await
}
