use engine::{CoreAgentEvent, ToolEvent};
use temporalio_sdk::WorkflowContext;

use crate::{
    ActiveEnvironmentJobWait, CheckEnvironmentJobWaitActivityResult,
    ENVIRONMENT_JOB_WAIT_DIRECTIVE_KIND, EnvironmentJobChanged, EnvironmentJobWaitDirective,
    PendingToolBatchResume,
};

use super::{AgentSessionWorkflow, check_environment_job_wait, workflow_time_ms};

const INITIAL_POLL_DELAY_MS: u64 = 2_000;
const FAST_POLL_ATTEMPTS: u32 = 30;
const MEDIUM_POLL_ATTEMPTS: u32 = 70;
const MEDIUM_POLL_DELAY_MS: u64 = 15_000;
const MAX_POLL_DELAY_MS: u64 = 60_000;

#[derive(Clone, Debug)]
pub(super) struct DeferredEnvironmentJobWait {
    run_id: engine::RunId,
    turn_id: engine::TurnId,
    batch_id: engine::ToolBatchId,
    directive: EnvironmentJobWaitDirective,
}

pub(super) fn directive_for_event(
    event: &CoreAgentEvent,
) -> anyhow::Result<Option<DeferredEnvironmentJobWait>> {
    let CoreAgentEvent::Tool(ToolEvent::BatchDeferred {
        run_id,
        turn_id,
        batch_id,
        resume_directive,
    }) = event
    else {
        return Ok(None);
    };
    if resume_directive.api_kind != ENVIRONMENT_JOB_WAIT_DIRECTIVE_KIND {
        return Ok(None);
    }
    let directive: EnvironmentJobWaitDirective =
        serde_json::from_value(resume_directive.body.clone()).map_err(|error| {
            anyhow::anyhow!("invalid environment job_wait resume directive: {error}")
        })?;
    Ok(Some(DeferredEnvironmentJobWait {
        run_id: *run_id,
        turn_id: *turn_id,
        batch_id: *batch_id,
        directive,
    }))
}

pub(super) fn record_changed(workflow: &mut AgentSessionWorkflow, changed: EnvironmentJobChanged) {
    for wait in workflow.active_environment_job_waits.values_mut() {
        if wait.handles.iter().any(|handle| {
            handle.session_id == changed.session_id
                && handle.env_id == changed.env_id
                && handle.job_id == changed.job_id
        }) {
            wait.next_check_at_ms = 0;
        }
    }
}

pub(super) fn has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    state
        .active_environment_job_waits
        .values()
        .any(|wait| wait.next_check_at_ms == 0)
}

pub(super) fn nearest_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .active_environment_job_waits
        .values()
        .flat_map(|wait| [Some(wait.next_check_at_ms), wait.deadline_ms])
        .flatten()
        .min()
}

pub(super) fn install(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    deferred: DeferredEnvironmentJobWait,
) {
    let now = workflow_time_ms(ctx);
    let deadline_ms = deferred
        .directive
        .timeout_ms
        .map(|timeout_ms| now.saturating_add(timeout_ms));
    let mut next_check_at_ms = now.saturating_add(poll_delay_ms(0));
    if let Some(deadline_ms) = deadline_ms {
        next_check_at_ms = next_check_at_ms.min(deadline_ms);
    }
    let wait = ActiveEnvironmentJobWait {
        batch_id: deferred.batch_id,
        run_id: deferred.run_id,
        turn_id: deferred.turn_id,
        call_id: deferred.directive.call_id,
        handles: deferred.directive.handles,
        mode: deferred.directive.mode,
        terminal_policy: deferred.directive.terminal_policy,
        output_bytes: deferred.directive.output_bytes,
        include_artifacts: deferred.directive.include_artifacts,
        deadline_ms,
        next_check_at_ms,
        poll_attempt: 0,
    };
    ctx.state_mut(|state| {
        state
            .active_environment_job_waits
            .insert(wait.batch_id.as_u64(), wait);
    });
}

pub(super) fn advance(wait: &mut ActiveEnvironmentJobWait, now_ms: u64) {
    wait.poll_attempt = wait.poll_attempt.saturating_add(1);
    wait.next_check_at_ms = now_ms.saturating_add(poll_delay_ms(wait.poll_attempt));
    if let Some(deadline_ms) = wait.deadline_ms {
        wait.next_check_at_ms = wait.next_check_at_ms.min(deadline_ms);
    }
}

pub(super) async fn process_due(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let due_waits = ctx.state_mut(|state| {
        let due_batch_ids = state
            .active_environment_job_waits
            .iter()
            .filter_map(|(batch_id, wait)| is_due(wait, now).then_some(*batch_id))
            .collect::<Vec<_>>();
        due_batch_ids
            .into_iter()
            .filter_map(|batch_id| state.active_environment_job_waits.remove(&batch_id))
            .collect::<Vec<_>>()
    });

    for mut wait in due_waits {
        match check_environment_job_wait(ctx, wait.clone(), now).await? {
            CheckEnvironmentJobWaitActivityResult::Ready { result } => {
                if result.batch_id != wait.batch_id {
                    anyhow::bail!(
                        "environment job wait check returned batch {} for active batch {}",
                        result.batch_id,
                        wait.batch_id
                    );
                }
                ctx.state_mut(|state| {
                    state
                        .pending_tool_batch_resumes
                        .push(PendingToolBatchResume {
                            batch_id: result.batch_id,
                            result,
                        });
                });
            }
            CheckEnvironmentJobWaitActivityResult::Pending => {
                advance(&mut wait, now);
                ctx.state_mut(|state| {
                    state
                        .active_environment_job_waits
                        .insert(wait.batch_id.as_u64(), wait);
                });
            }
        }
    }
    Ok(())
}

fn poll_delay_ms(poll_attempt: u32) -> u64 {
    if poll_attempt < FAST_POLL_ATTEMPTS {
        INITIAL_POLL_DELAY_MS
    } else if poll_attempt < MEDIUM_POLL_ATTEMPTS {
        MEDIUM_POLL_DELAY_MS
    } else {
        MAX_POLL_DELAY_MS
    }
}

fn is_due(wait: &ActiveEnvironmentJobWait, now_ms: u64) -> bool {
    wait.next_check_at_ms <= now_ms || wait.deadline_ms.is_some_and(|deadline| deadline <= now_ms)
}
