use super::*;

impl GatewayAgentApi {
    pub(super) async fn selectable_environment_for_session(
        &self,
        state: &harness::CoreAgentState,
        environment_id: &harness::EnvironmentId,
    ) -> Result<::environments::EnvironmentRecord, AgentApiError> {
        let feature = state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.environments.as_ref())
            .ok_or_else(|| {
                AgentApiError::rejected(
                    "environment activation requires the environments feature to be granted",
                )
            })?;
        if !feature.is_attached(environment_id.as_str()) {
            return Err(AgentApiError::rejected(format!(
                "environment {environment_id} is not attached to this session"
            )));
        }
        crate::environments::resolver::EnvironmentResolver::from_pg_store(self.store.clone())
            .selectable(environment_id)
            .await
            .map_err(map_environment_resolve_error)
    }

    pub(super) async fn wait_for_active_environment(
        &self,
        session_id: &SessionId,
        expected: Option<&harness::EnvironmentId>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for active environment update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures
                    && let Some(failure) = status.admission_failures.last()
                {
                    return Err(map_admission_failure_to_api_error(failure));
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded.state.environment.active_environment_id.as_ref() == expected {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

/// One API mapping for resolver failures everywhere the gateway resolves an
/// environment on a caller's behalf. `NotReady` and `Unreachable` are typed
/// `environment_not_ready` so a wait is distinguishable from rejection: the
/// environment is provisioning, waking on its data route, its daemon is
/// reconnecting, or the dial failed without a typed refusal, and the
/// caller's correct move is retry-with-backoff, not failure.
pub(super) fn map_environment_resolve_error(
    error: crate::environments::resolver::EnvironmentResolveError,
) -> AgentApiError {
    use crate::environments::resolver::EnvironmentResolveError;
    match error {
        EnvironmentResolveError::Store(error) => map_environments_error(error),
        EnvironmentResolveError::NotReady {
            environment_id,
            reason,
            retry_after_ms,
            ..
        } => AgentApiError::environment_not_ready(format!(
            "environment {environment_id} is not ready ({reason}); retry after {retry_after_ms} ms"
        )),
        EnvironmentResolveError::Unreachable {
            environment_id,
            status,
        } => AgentApiError::environment_not_ready(format!(
            "environment {environment_id} is unreachable ({status}); retry with backoff"
        )),
        other => AgentApiError::rejected(other.to_string()),
    }
}

pub(super) fn activate_environment_command(
    environment_id: harness::EnvironmentId,
) -> CoreAgentCommand {
    CoreAgentCommand::SetActiveEnvironment { environment_id }
}

pub(super) fn deactivate_environment_command() -> CoreAgentCommand {
    CoreAgentCommand::ClearActiveEnvironment
}

#[cfg(test)]
mod tests {
    use api::AgentApiErrorKind;

    use super::map_environment_resolve_error;
    use crate::environments::resolver::EnvironmentResolveError;

    #[test]
    fn an_unreachable_environment_is_not_ready_and_a_failed_one_is_rejected() {
        let unreachable = map_environment_resolve_error(EnvironmentResolveError::Unreachable {
            environment_id: "env-1".to_owned(),
            status: "connect refused".to_owned(),
        });
        assert_eq!(unreachable.kind, AgentApiErrorKind::EnvironmentNotReady);

        let failed = map_environment_resolve_error(EnvironmentResolveError::Failed {
            environment_id: "env-1".to_owned(),
            message: "boot failed".to_owned(),
        });
        assert_eq!(failed.kind, AgentApiErrorKind::Rejected);
    }
}
