use super::*;

pub(super) fn record_admission_failure(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    failure: AgentAdmissionFailure,
) {
    ctx.state_mut(|state| state.admission_failures.push(failure));
}

pub(super) fn record_warning(ctx: &WorkflowContext<AgentSessionWorkflow>, warning: String) {
    let replaying = ctx.is_replaying();
    ctx.state_mut(|state| state.record_warning(replaying, warning));
}

/// Record why the workflow is about to fail. Every caller returns the error
/// right after, so a set `fatal_error` always means the execution is ending.
pub(super) fn record_error(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    error: &anyhow::Error,
    error_class: &'static str,
) {
    let message = error.to_string();
    ctx.state_mut(|state| {
        state.fatal_error = Some(message.clone());
    });
    observability::log_workflow_failure(ctx, error_class, &message);
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
        state.fatal_error = Some(message.clone());
        state.bootstrap_failed = true;
    });
    observability::log_workflow_failure(ctx, "bootstrap", &message);
}
