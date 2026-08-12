use super::*;

impl GatewayAgentApi {
    pub(super) async fn selectable_environment_for_session(
        &self,
        state: &engine::CoreAgentState,
        environment_id: &engine::EnvironmentId,
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
        let allowed = feature
            .providers
            .as_ref()
            .map(|providers| providers.iter().cloned().collect::<BTreeSet<_>>());
        crate::environment_resolver::EnvironmentResolver::from_pg_store(self.store.clone())
            .with_routes(self.environment_routes.clone())
            .selectable(environment_id, allowed.as_ref(), now_ms()?)
            .await
            .map_err(|error| match error {
                crate::environment_resolver::EnvironmentResolveError::Store(error) => {
                    map_environments_error(error)
                }
                other => AgentApiError::rejected(other.to_string()),
            })
    }

    pub(super) async fn wait_for_active_environment(
        &self,
        session_id: &SessionId,
        expected: Option<&engine::EnvironmentId>,
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
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
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

pub(super) fn activate_environment_command(
    environment_id: engine::EnvironmentId,
) -> CoreAgentCommand {
    CoreAgentCommand::SetActiveEnvironment { environment_id }
}

pub(super) fn deactivate_environment_command() -> CoreAgentCommand {
    CoreAgentCommand::ClearActiveEnvironment
}
