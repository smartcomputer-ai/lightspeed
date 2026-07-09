use super::*;

pub(super) fn record_admission_failure(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    failure: AgentAdmissionFailure,
) {
    ctx.state_mut(|state| {
        state.admission_failures.push(failure);
        state.last_error = None;
    });
}

pub(super) fn record_error(ctx: &WorkflowContext<AgentSessionWorkflow>, error: &anyhow::Error) {
    let message = error.to_string();
    ctx.state_mut(|state| {
        state.last_error = Some(message);
    });
}

/// Record a failure that occurred during session bootstrap (rehydration). This
/// is surfaced distinctly from ordinary run errors so the gateway/bridge can
/// report a typed `session_bootstrap_failed` recovery problem instead of a
/// generic message-answer failure.
pub(super) fn record_bootstrap_error(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    error: &anyhow::Error,
) {
    let message = error.to_string();
    ctx.state_mut(|state| {
        state.last_error = Some(message);
        state.bootstrap_failed = true;
    });
}
