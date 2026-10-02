//! Active-run control: client admissions — cancel, steer, queue —
//! must reach the engine while the drive loop is executing an activity, not
//! after the run ends. This module holds the one primitive the drive loop and
//! the tool-batch executor use to race an in-flight activity against pending
//! admissions, admit them against the live drive, and abandon the activity
//! when the engine no longer wants its result.

use temporalio_sdk::CancellableFuture;

use super::*;

/// Outcome of racing one activity against client admissions.
pub(super) enum Raced<T> {
    /// The activity finished; its result is still wanted by the engine.
    Completed(T),
    /// Admissions drained mid-flight made the work obsolete (the run is
    /// cancelling, or the turn/batch is no longer the engine's active one).
    /// The activity was cancelled and its eventual result discarded; the
    /// caller re-plans from the drive.
    Preempted,
}

/// Await `activity` while draining client admissions as they arrive. After
/// each drain, `still_wanted` decides against the live engine state whether
/// the activity's result still matters; if not, the activity is cancelled
/// (`TryCancel`: the future resolves at once, the worker learns through its
/// heartbeat) and `Preempted` is returned.
///
/// During compaction, only cancellation admissions can pass its frozen revision.
pub(super) async fn race_activity_with_admissions<T, F>(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    activity: F,
    still_wanted: impl Fn(&CoreAgentState) -> bool,
) -> anyhow::Result<Raced<T>>
where
    F: CancellableFuture<T>,
{
    pin_mut!(activity);
    loop {
        let admissions_pending = {
            let wait = ctx.wait_condition(admissions::has_admissible_admissions);
            pin_mut!(wait);
            select! {
                result = activity => return Ok(Raced::Completed(result)),
                _ = wait => true,
            }
        };
        debug_assert!(admissions_pending);
        if !admissions::drain_pending_admissions(ctx, drive).await? {
            // Left queued on purpose (pending compaction); nothing to re-plan
            // and the wait condition would fire again immediately, so fall
            // back to awaiting the activity alone.
            let result = activity.await;
            return Ok(Raced::Completed(result));
        }
        if still_wanted(drive.state()) {
            continue;
        }
        activity.as_ref().get_ref().cancel();
        // `TryCancel` resolves the future immediately; a result that raced
        // the cancellation is discarded either way.
        let _ = activity.await;
        return Ok(Raced::Preempted);
    }
}

/// The generation for `turn_id` of `run_id` is still what the engine wants.
pub(super) fn generation_still_wanted(
    state: &CoreAgentState,
    run_id: engine::RunId,
    turn_id: engine::TurnId,
) -> bool {
    state.runs.active.as_ref().is_some_and(|run| {
        run.run_id == run_id
            && run.status == RunStatus::Active
            && run.active_turn_id == Some(turn_id)
    })
}

/// The tool batch `batch_id` of `run_id` is still executing for the engine.
pub(super) fn tool_batch_still_wanted(
    state: &CoreAgentState,
    run_id: engine::RunId,
    batch_id: engine::ToolBatchId,
) -> bool {
    state.runs.active.as_ref().is_some_and(|run| {
        run.run_id == run_id
            && run.status == RunStatus::Active
            && run.active_tool_batch_id == Some(batch_id)
    })
}
