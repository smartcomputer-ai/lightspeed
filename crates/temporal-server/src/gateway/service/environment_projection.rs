use super::*;

impl GatewayAgentApi {
    pub(super) async fn refresh_environment_projection_for_idle_session(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<(), AgentApiError> {
        if state.lifecycle.status != CoreAgentStatus::Open
            || state.runs.active.is_some()
            || !state.runs.queued.is_empty()
        {
            return Ok(());
        }

        let commands = self
            .environment_projection_refresh_commands(session_id, state)
            .await?;
        if commands.is_empty() {
            return Ok(());
        }

        let expected = commands
            .iter()
            .filter_map(|command| match command {
                CoreAgentCommand::UpsertContext { key, entry } => {
                    Some((key.clone(), entry.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        for command in commands {
            self.submit_core_command(session_id, command).await?;
        }
        if !expected.is_empty() {
            self.wait_for_context_entries_applied(session_id, &expected, baseline_failures)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn environment_projection_refresh_commands(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<Vec<CoreAgentCommand>, AgentApiError> {
        let manager = crate::environment::SessionEnvironmentManager::new(
            self.store.clone(),
            self.store.clone(),
        );
        let refresh = manager
            .refresh_projection(session_id, state)
            .await
            .map_err(map_session_environment_error)?;
        Ok(refresh.commands)
    }
}

fn map_session_environment_error(
    error: crate::environment::SessionEnvironmentManagerError,
) -> AgentApiError {
    match error {
        crate::environment::SessionEnvironmentManagerError::VfsCatalog(error) => {
            map_vfs_catalog_error(error)
        }
        other => AgentApiError::internal(other.to_string()),
    }
}
