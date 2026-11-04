use temporalio_common::error::{ApplicationFailure, IncomingError};
use temporalio_sdk::ActivityExecutionError;

use super::*;

/// The session store refused an append because of the data in it. Nothing was written
/// and the state did not move, so the drive can discard the events and fail the run.
#[derive(Debug)]
pub(super) struct StoreDataRejection {
    pub(super) message: String,
}

impl std::fmt::Display for StoreDataRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreDataRejection {}

fn application_failure(error: &ActivityExecutionError) -> Option<&ApplicationFailure> {
    let mut cause = error.cause();
    while let Some(incoming) = cause {
        if let IncomingError::Application(failure) = incoming {
            return Some(failure);
        }
        cause = incoming.cause();
    }
    None
}

/// The worker's own words for an activity failure. An activity failure's top message
/// only says that the task failed; what went wrong is in its cause chain.
pub(crate) fn activity_failure_message(error: &ActivityExecutionError) -> String {
    application_failure(error).map_or_else(|| error.to_string(), ToString::to_string)
}

/// Why an append failed, keeping a data rejection recognizable to the drive's failure
/// path. Every other failure stays untyped and fatal.
pub(super) fn append_failure(error: &ActivityExecutionError) -> anyhow::Error {
    let message = activity_failure_message(error);
    let rejected = application_failure(error).is_some_and(|failure| {
        failure.type_name() == Some(crate::SESSION_DATA_REJECTED_ERROR_TYPE)
    });
    if rejected {
        anyhow::Error::new(StoreDataRejection { message })
    } else {
        anyhow::anyhow!(message)
    }
}

pub(super) fn record_admission_failure(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    failure: AgentAdmissionFailure,
) {
    ctx.state_mut(|state| state.record_admission_failure(failure));
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

/// [`record_error`] for the failure the workflow returns next.
pub(super) fn fatal(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    error: anyhow::Error,
    error_class: &'static str,
) -> temporalio_sdk::ApplicationFailure {
    record_error(ctx, &error, error_class);
    temporalio_sdk::ApplicationFailure::new(error)
}

/// Record a failure that occurred during session bootstrap (rehydration). This
/// is surfaced distinctly from ordinary run errors so the gateway/bridge can
/// report a typed `sessionBootstrapFailed` recovery problem instead of a
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
