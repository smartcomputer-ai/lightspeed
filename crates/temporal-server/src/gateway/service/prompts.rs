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

        self.load_session_state(session_id).await
    }

    pub(super) async fn refresh_prompt_instructions_for_idle_session(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<(), AgentApiError> {
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
        Ok(())
    }

    pub(super) async fn prompt_instruction_source_map(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<BTreeMap<ContextEntryKey, ContextEntryInput>, AgentApiError> {
        let prompts_config = state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .and_then(|vfs| vfs.prompts.as_ref());
        let mounts = if prompts_config.is_some() {
            self.store
                .list_mounts(session_id)
                .await
                .map_err(map_vfs_catalog_error)?
        } else {
            Vec::new()
        };
        let specs = match prompts_config {
            Some(config) => {
                tools::prompts::configured_vfs_prompt_root_specs(&mounts, config.roots.as_deref())
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?
            }
            None => Vec::new(),
        };
        if specs.is_empty() {
            let publication = tools::prompts::prepare_prompt_instructions_publication(
                self.store.as_ref(),
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
            tools::prompts::resolve_mounted_vfs_prompt_roots(blobs, workspace_store, mounts, specs)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let publication = tools::prompts::prepare_prompt_instructions_publication(
            self.store.as_ref(),
            &inputs,
            tools::prompts::PromptAssemblyLimits::default(),
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

#[cfg(test)]
pub(super) fn prompt_report_ref(entry: &ContextEntry) -> Result<Option<BlobRef>, AgentApiError> {
    if entry.provider_kind.as_deref() != Some(tools::prompts::PROMPT_INSTRUCTIONS_PROVIDER_KIND) {
        return Ok(None);
    }
    let Some(value) = entry.provider_item_id.as_deref() else {
        return Ok(None);
    };
    BlobRef::parse(value).map(Some).map_err(|error| {
        AgentApiError::internal(format!("stored prompt report ref is invalid: {error}"))
    })
}
