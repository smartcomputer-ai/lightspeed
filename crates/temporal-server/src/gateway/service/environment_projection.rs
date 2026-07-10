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
                CoreAgentCommand::UpsertContext { key, entry, .. } => {
                    Some((key.clone(), entry.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let removed = commands
            .iter()
            .filter_map(|command| match command {
                CoreAgentCommand::RemoveContext { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut correlations = BTreeMap::new();
        for command in commands {
            correlations.extend(
                self.submit_correlated_context_commands(session_id, vec![command])
                    .await?,
            );
        }
        if !expected.is_empty() {
            self.wait_for_context_entries_applied(session_id, &expected, &correlations)
                .await?;
        }
        if !removed.is_empty() {
            let (_, outcomes) = self
                .wait_for_context_keys_removed(session_id, &removed, &correlations)
                .await?;
            if let Some(failure) = outcomes.into_values().flatten().next() {
                return Err(map_admission_failure_to_api_error(&failure));
            }
        }
        Ok(())
    }

    pub(super) async fn environment_projection_refresh_commands(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<Vec<CoreAgentCommand>, AgentApiError> {
        let features = state
            .lifecycle
            .config
            .as_ref()
            .map(|config| &config.features);
        let vfs_catalog_enabled = features.is_some_and(|features| features.vfs.is_some());
        let environment_catalog_enabled =
            features.is_some_and(|features| features.environments.is_some());
        let mounts = if vfs_catalog_enabled {
            self.store
                .list_mounts(session_id)
                .await
                .map_err(map_vfs_catalog_error)?
        } else {
            Vec::new()
        };
        let environments = if environment_catalog_enabled {
            self.load_session_runtime_environments(session_id).await?
        } else {
            Vec::new()
        };
        let refresh = self
            .environment_manager
            .refresh_projection_for_runtime_environments(
                state,
                mounts,
                environments,
                vfs_catalog_enabled,
                environment_catalog_enabled,
            )
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
