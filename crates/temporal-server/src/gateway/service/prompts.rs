use std::sync::Arc;

use super::*;

impl GatewayAgentApi {
    pub(super) async fn load_session_state_with_current_run_context(
        &self,
        session_id: &SessionId,
    ) -> Result<LoadedSession, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open
            || loaded.state.runs.active.is_some()
            || !loaded.state.runs.queued.is_empty()
        {
            return Ok(loaded);
        }

        self.refresh_environment_projection_for_idle_session(session_id, &loaded.state)
            .await?;

        let loaded = self.load_session_state(session_id).await?;
        self.refresh_prompt_instructions_for_idle_session(session_id, &loaded.state)
            .await?;

        let loaded = self.load_session_state(session_id).await?;
        self.refresh_skill_catalog_for_idle_session(session_id, &loaded.state)
            .await?;

        let loaded = self.load_session_state(session_id).await?;
        self.refresh_subagent_catalog_for_idle_session(session_id, &loaded.state)
            .await?;

        self.load_session_state(session_id).await
    }

    pub(super) async fn refresh_prompt_instructions_for_idle_session(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<(), AgentApiError> {
        if state.runs.active.is_some() || !state.runs.queued.is_empty() {
            return Ok(());
        }
        let desired = self
            .prompt_instruction_source_map(session_id, state)
            .await?;
        self.reconcile_managed_instructions(
            session_id,
            state,
            tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX,
            desired,
        )
        .await?;
        let loaded = self.load_session_state(session_id).await?;
        let resolver =
            crate::environment_resolver::EnvironmentResolver::from_pg_store(self.store.clone());
        let desired = crate::environment_prompts::refresh(
            self.store.as_ref(),
            Some(&resolver),
            Some(&self.environment_gateway),
            loaded
                .state
                .lifecycle
                .config
                .as_ref()
                .and_then(|config| config.features.environments.as_ref()),
            loaded.state.environment.active_environment_id.as_ref(),
        )
        .await
        .map_err(|e| AgentApiError::internal(e.to_string()))?;
        self.reconcile_managed_instructions(
            session_id,
            &loaded.state,
            tools::prompts::environment::ENVIRONMENT_PROMPT_CONTEXT_KEY,
            desired,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn prompt_instruction_source_map(
        &self,
        _session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<BTreeMap<ContextEntryKey, ContextEntryInput>, AgentApiError> {
        let prompts_config = state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .and_then(|vfs| vfs.prompts.as_ref());
        let links = if prompts_config.is_some() {
            self.resolve_session_workspace_links(state).await?
        } else {
            Vec::new()
        };
        let specs = match prompts_config {
            Some(config) => {
                tools::prompts::configured_vfs_prompt_root_specs(&links, config.roots.as_deref())
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?
            }
            None => Vec::new(),
        };
        if specs.is_empty() {
            let publication = tools::prompts::prepare_prompt_instructions_publication(
                self.store.as_ref(),
                Some(self.store.as_ref()),
                &[],
                tools::prompts::PromptAssemblyLimits::default(),
            )
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
            return Ok(publication.desired);
        }

        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let resolved =
            tools::prompts::resolve_linked_vfs_prompt_roots(blobs, workspace_store, links, specs)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let publication = tools::prompts::prepare_prompt_instructions_publication_with_warnings(
            self.store.as_ref(),
            Some(self.store.as_ref()),
            &inputs,
            tools::prompts::PromptAssemblyLimits::default(),
            resolved.warnings().to_vec(),
        )
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
        Ok(publication.desired)
    }
}

#[cfg(test)]
pub(super) fn active_prompt_context_entries(state: &engine::CoreAgentState) -> Vec<&ContextEntry> {
    tools::prompts::active_prompt_instruction_entries(state)
}
