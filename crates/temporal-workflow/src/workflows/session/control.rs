//! Active-run control: client admissions — cancel, steer, queue —
//! must reach the harness while the drive loop is executing an activity, not
//! after the run ends. This module holds the one primitive the drive loop and
//! the tool-batch executor use to race an in-flight activity against pending
//! admissions, admit them against the live drive, and abandon the activity
//! when the harness no longer wants its result.

use crate::workflows::WorkflowContextExt as _;
use temporalio_sdk::CancellableFuture;

use super::*;

/// Outcome of racing one activity against client admissions.
pub(super) enum Raced<T> {
    /// The activity finished; its result is still wanted by the harness.
    Completed(T),
    /// Admissions drained mid-flight made the work obsolete (the run is
    /// cancelling, or the turn/batch is no longer the harness's active one).
    /// The activity was cancelled and its eventual result discarded; the
    /// caller re-plans from the drive.
    Preempted,
}

/// Await `activity` while draining client admissions as they arrive. After
/// each drain, `still_wanted` decides against the live harness state whether
/// the activity's result still matters; if not, the activity is cancelled
/// (`TryCancel`: the future resolves at once, the worker learns through its
/// heartbeat) and `Preempted` is returned.
///
/// During compaction, only cancellation and force close can pass its frozen revision.
pub(super) async fn race_activity_with_admissions<T, F>(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    activity: F,
    still_wanted: impl Fn(&CoreAgentState) -> bool,
) -> anyhow::Result<Raced<T>>
where
    F: CancellableFuture<Output = T>,
{
    pin_mut!(activity);
    loop {
        let admissions_pending = {
            let wait = ctx.wait_for_state(admissions::has_admissible_admissions);
            pin_mut!(wait);
            // The result first: the drive commits it before it admits whatever
            // landed in the same workflow task, so finished work is never lost.
            select_biased! {
                result = activity => return Ok(Raced::Completed(result)),
                _ = wait => true,
            }
        };
        debug_assert!(admissions_pending);
        if !admissions::drain_pending_admissions(ctx, drive).await? {
            // The wait condition matches what the drain admits, so this is
            // defensive: nothing to re-plan, and waiting again would fire
            // immediately, so fall back to awaiting the activity alone.
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

/// The generation for `turn_id` of `run_id` is still what the harness wants.
pub(super) fn generation_still_wanted(
    state: &CoreAgentState,
    run_id: harness::RunId,
    turn_id: harness::TurnId,
) -> bool {
    state.runs.active.as_ref().is_some_and(|run| {
        run.run_id == run_id
            && run.status == RunStatus::Running
            && run.active_turn_id == Some(turn_id)
    })
}

/// New work of the tool batch `batch_id` of `run_id` may still start: the run is
/// active on it, so neither a cancel nor a park has intervened.
pub(super) fn tool_batch_still_wanted(
    state: &CoreAgentState,
    run_id: harness::RunId,
    batch_id: harness::ToolBatchId,
) -> bool {
    state.runs.active.as_ref().is_some_and(|run| {
        run.run_id == run_id
            && run.status == RunStatus::Running
            && run.active_tool_batch_id == Some(batch_id)
    })
}

/// Calls already running for the tool batch `batch_id` of `run_id` have nobody to
/// report to: the run is gone, it moved on to another batch, or it is ending. A park
/// is none of these: a dispatch only parks once its window has drained, and a call
/// whose result was thrown away for a park would run again after the decision.
pub(super) fn tool_batch_abandoned(
    state: &CoreAgentState,
    run_id: harness::RunId,
    batch_id: harness::ToolBatchId,
) -> bool {
    !state.runs.active.as_ref().is_some_and(|run| {
        run.run_id == run_id
            && matches!(run.status, RunStatus::Running | RunStatus::Parked)
            && run.active_tool_batch_id == Some(batch_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_run(status: RunStatus, batch: Option<u64>) -> CoreAgentState {
        let mut state = CoreAgentState::default();
        state.runs.active = Some(harness::ActiveRun {
            context_recovery: Default::default(),
            run_id: harness::RunId::new(1),
            status,
            submission_id: None,
            source: harness::RunSource::Input { input: Vec::new() },
            input_entry_ids: Vec::new(),
            input_consumed_by_turn_id: None,
            run_config: crate::default_run_config(),
            input_limit_tokens: None,
            config_revision: 0,
            first_seq: harness::EventSeq::new(1),
            accepted_at_ms: 1,
            started_at_ms: Some(1),
            usage: None,
            steering: Vec::new(),
            turns: Default::default(),
            turn_count: 0,
            entry_guard_attempted: false,
            active_turn_id: None,
            active_tool_batch_id: batch.map(harness::ToolBatchId::new),
            approvals: Default::default(),
            parked_tool_batch: None,
            tool_batches: Default::default(),
            completed_tool_batches: Default::default(),
            notify_on_terminal: Vec::new(),
        });
        state
    }

    fn abandoned(state: &CoreAgentState) -> bool {
        tool_batch_abandoned(state, harness::RunId::new(1), harness::ToolBatchId::new(7))
    }

    #[test]
    fn a_park_keeps_in_flight_calls_but_stops_new_ones() {
        let parked = state_with_run(RunStatus::Parked, Some(7));
        assert!(!abandoned(&parked));
        assert!(!tool_batch_still_wanted(
            &parked,
            harness::RunId::new(1),
            harness::ToolBatchId::new(7)
        ));
        assert!(!abandoned(&state_with_run(RunStatus::Running, Some(7))));
    }

    #[test]
    fn an_ending_run_or_a_moved_batch_abandons_in_flight_calls() {
        assert!(abandoned(&CoreAgentState::default()));
        assert!(abandoned(&state_with_run(RunStatus::Cancelling, Some(7))));
        assert!(abandoned(&state_with_run(RunStatus::Running, Some(8))));
        assert!(abandoned(&state_with_run(RunStatus::Running, None)));
        let mut other_run = state_with_run(RunStatus::Running, Some(7));
        other_run.runs.active.as_mut().expect("run").run_id = harness::RunId::new(2);
        assert!(abandoned(&other_run));
    }
}
