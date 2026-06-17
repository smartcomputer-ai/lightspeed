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
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let vfs_catalog = tools::environment::projection::vfs_catalog_from_mounts(&mounts)
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let vfs_publication = tools::environment::projection::prepare_vfs_catalog_publication(
            self.store.as_ref(),
            state,
            vfs_catalog,
        )
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?;

        let environment_catalog = tools::environment::projection::empty_environment_catalog(0);
        let environment_publication =
            tools::environment::projection::prepare_environment_catalog_publication(
                self.store.as_ref(),
                state,
                environment_catalog,
            )
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;

        let mut commands = Vec::new();
        if let Some(command) = vfs_publication.command {
            commands.push(command);
        }
        if let Some(command) = environment_publication.command {
            commands.push(command);
        }
        if let Some(command) = tools::environment::projection::clear_environment_active_command(
            tools::environment::projection::current_environment_active_ref(state).as_ref(),
        ) {
            commands.push(command);
        }
        Ok(commands)
    }
}
