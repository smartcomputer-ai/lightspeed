use temporal_workflow::{
    SubagentCloseActivityRequest, SubagentLivenessActivityRequest, SubagentLivenessActivityResult,
    SubagentPrepareActivityRequest, SubagentPrepareActivityResult, SubagentResolveActivityRequest,
};
use temporalio_sdk::activities::ActivityError;

use super::{
    common::{activity_error, non_retryable_activity_error},
    state::SubagentActivityDeps,
};

/// A refusal gets the same answer on every attempt, so the execution should hear
/// it now. What can pass is retried within the activity's budget.
fn subagent_failure(error: api::AgentApiError) -> ActivityError {
    use api::AgentApiErrorKind as Kind;
    match error.kind {
        Kind::InvalidRequest
        | Kind::NotFound
        | Kind::Forbidden
        | Kind::Rejected
        | Kind::Unauthenticated
        | Kind::ModelDefaultUnset
        | Kind::AudioBlobTooLarge
        | Kind::ResponseTooLarge
        | Kind::SessionExecutionStopped => non_retryable_activity_error(error),
        Kind::Conflict
        | Kind::EnvironmentNotReady
        | Kind::SessionBootstrapFailed
        | Kind::OperationPending
        | Kind::Internal => activity_error(error),
    }
}

pub(super) async fn prepare(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentPrepareActivityRequest,
) -> Result<SubagentPrepareActivityResult, ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default();
    deps.service
        .prepare(request.start, now_ms)
        .await
        .map_err(subagent_failure)
}

pub(super) async fn resolve(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentResolveActivityRequest,
) -> Result<harness::PromiseResolution, ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    deps.service
        .resolve(request.child, request.terminal)
        .await
        .map_err(subagent_failure)
}

pub(super) async fn close(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentCloseActivityRequest,
) -> Result<(), ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    deps.service
        .close(&request.session_id)
        .await
        .map_err(|error| {
            tracing::warn!(
                session_id = %request.session_id,
                %error,
                "could not close a sub-agent child; it holds a delegation slot until force closed"
            );
            subagent_failure(error)
        })
}

pub(super) async fn liveness(
    deps: Option<&SubagentActivityDeps>,
    request: SubagentLivenessActivityRequest,
) -> Result<SubagentLivenessActivityResult, ActivityError> {
    let deps = deps
        .ok_or_else(|| activity_error(anyhow::anyhow!("subagent activities are not configured")))?;
    deps.service
        .liveness(&request.session_id)
        .await
        .map_err(subagent_failure)
}

#[cfg(test)]
mod tests {
    use api::AgentApiError;
    use temporalio_sdk::ApplicationFailure;

    use super::*;

    fn failure(error: AgentApiError) -> ApplicationFailure {
        match subagent_failure(error) {
            ActivityError::Application(failure) => *failure,
            other => panic!("expected an application failure, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_is_final_and_an_outage_retries() {
        for error in [
            AgentApiError::invalid_request("bad"),
            AgentApiError::not_found("gone"),
            AgentApiError::forbidden(),
            AgentApiError::rejected("refused"),
            AgentApiError::model_default_unset(api::ModelDefaultSlot::AgentRun),
        ] {
            assert!(failure(error).is_non_retryable());
        }
        for error in [
            AgentApiError::internal("store down"),
            AgentApiError::conflict("head moved"),
        ] {
            assert!(!failure(error).is_non_retryable());
        }
    }
}
