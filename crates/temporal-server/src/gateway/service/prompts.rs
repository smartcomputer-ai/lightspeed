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
        self.refresh_skill_catalog_for_idle_session(
            session_id,
            active_skill_catalog_ref(&loaded.state),
        )
        .await?;

        self.load_session_state(session_id).await
    }

    pub(super) async fn refresh_prompt_instructions_for_idle_session(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<(), AgentApiError> {
        let Some(command) = self
            .prompt_instructions_refresh_command(session_id, state)
            .await?
        else {
            return Ok(());
        };
        let target_prompt_refs = match &command {
            CoreAgentCommand::ReplaceContextPrefix {
                key_prefix,
                entries,
            } if key_prefix.as_str() == tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX => {
                entries
                    .iter()
                    .map(|(key, entry)| (key.clone(), entry.content_ref.clone()))
                    .collect::<Vec<_>>()
            }
            _ => {
                return Err(AgentApiError::internal(
                    "prompt refresh produced non-prompt context command",
                ));
            }
        };
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(session_id, command).await?;
        self.wait_for_prompt_instructions(session_id, target_prompt_refs, baseline_failures)
            .await
    }

    pub(super) async fn prompt_instructions_refresh_command(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<Option<CoreAgentCommand>, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let specs = tools::prompts::conventional_vfs_prompt_root_specs(&mounts);
        if specs.is_empty() {
            let publication = tools::prompts::prepare_prompt_instructions_publication(
                self.store.as_ref(),
                state,
                &[],
                tools::prompts::PromptAssemblyLimits::default(),
            )
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
            return Ok(publication.command);
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
            state,
            &inputs,
            tools::prompts::PromptAssemblyLimits::default(),
        )
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
        Ok(publication.command)
    }

    pub(super) async fn wait_for_prompt_instructions(
        &self,
        session_id: &SessionId,
        target_prompt_refs: Vec<(ContextEntryKey, BlobRef)>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for prompt instructions update: {session_id}"
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
            if sorted_prompt_refs(tools::prompts::active_prompt_instruction_refs(
                &loaded.state,
            )) == sorted_prompt_refs(target_prompt_refs.clone())
            {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn project_active_prompts(
        &self,
        loaded: &LoadedSession,
    ) -> Result<PromptsActiveResponse, AgentApiError> {
        let mut entries = active_prompt_context_entries(&loaded.state);
        entries.sort_by(|left, right| left.key.cmp(&right.key));
        let report_ref = prompt_report_ref_for_entries(&entries)?;
        let report = match report_ref.as_ref() {
            Some(report_ref) => Some(self.read_prompt_report(report_ref).await?),
            None => None,
        };

        Ok(PromptsActiveResponse {
            instructions: entries
                .into_iter()
                .filter_map(prompt_instruction_view)
                .collect(),
            report_ref: report_ref.map(|report_ref| report_ref.as_str().to_owned()),
            report,
        })
    }

    pub(super) async fn read_prompt_report(
        &self,
        report_ref: &BlobRef,
    ) -> Result<serde_json::Value, AgentApiError> {
        let bytes = self
            .store
            .read_bytes(report_ref)
            .await
            .map_err(map_blob_read_error)?;
        serde_json::from_slice(&bytes).map_err(|error| {
            AgentApiError::internal(format!(
                "stored prompt instructions report is invalid JSON: {error}"
            ))
        })
    }
}

fn sorted_prompt_refs(
    mut refs: Vec<(ContextEntryKey, BlobRef)>,
) -> Vec<(ContextEntryKey, BlobRef)> {
    refs.sort_by(|left, right| left.0.cmp(&right.0));
    refs
}

pub(super) fn active_prompt_context_entries(state: &engine::CoreAgentState) -> Vec<&ContextEntry> {
    tools::prompts::active_prompt_instruction_entries(state)
}

pub(super) fn prompt_report_ref_for_entries(
    entries: &[&ContextEntry],
) -> Result<Option<BlobRef>, AgentApiError> {
    let mut report_ref = None;
    for entry in entries {
        let Some(next) = prompt_report_ref(entry)? else {
            continue;
        };
        match report_ref.as_ref() {
            Some(existing) if existing != &next => {
                return Err(AgentApiError::internal(
                    "active prompt instruction entries reference multiple reports",
                ));
            }
            Some(_) => {}
            None => report_ref = Some(next),
        }
    }
    Ok(report_ref)
}

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

fn prompt_instruction_view(entry: &ContextEntry) -> Option<PromptInstructionView> {
    Some(PromptInstructionView {
        key: entry.key.as_ref()?.as_str().to_owned(),
        instructions_ref: entry.content_ref.as_str().to_owned(),
        media_type: entry.media_type.clone(),
        preview: entry.preview.clone(),
    })
}
