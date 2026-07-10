use super::*;

pub(super) const DEFAULT_INSTRUCTIONS_CONTEXT_KEY: &str = "instructions.000.default";
pub(super) const INSTRUCTIONS_CONTEXT_PREFIX: &str = "instructions";

impl GatewayAgentApi {
    pub(super) async fn reconcile_managed_instructions(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
        owned_prefix: &str,
        source_entries: BTreeMap<ContextEntryKey, ContextEntryInput>,
    ) -> Result<bool, AgentApiError> {
        if state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if state.runs.active.is_some() || !state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "managed instructions can only change while no run is active or queued",
            ));
        }

        let default_ref = self
            .store
            .as_ref()
            .put_bytes(
                temporal_workflow::default_instructions()
                    .as_bytes()
                    .to_vec(),
            )
            .await
            .map_err(map_blob_store_error)?;
        let desired = replace_managed_instruction_source(
            active_instruction_inputs(state),
            owned_prefix,
            source_entries,
            default_instruction_input(default_ref),
        )?;

        if active_instruction_inputs(state) == desired {
            return Ok(false);
        }

        let correlations = self
            .submit_correlated_context_commands(
                session_id,
                vec![CoreAgentCommand::ReplaceContextPrefix {
                    expected_revision: Some(state.context.revision),
                    key_prefix: ContextEntryKey::new(INSTRUCTIONS_CONTEXT_PREFIX),
                    entries: desired.clone(),
                }],
            )
            .await?;
        self.wait_for_managed_instruction_map(session_id, &desired, &correlations)
            .await?;
        Ok(true)
    }

    async fn wait_for_managed_instruction_map(
        &self,
        session_id: &SessionId,
        desired: &BTreeMap<ContextEntryKey, ContextEntryInput>,
        correlations: &BTreeMap<String, ContextEntryKey>,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for managed instructions update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(failure) = status.admission_failures.iter().find(|failure| {
                    failure
                        .correlation_token
                        .as_ref()
                        .is_some_and(|token| correlations.contains_key(token))
                }) {
                    return Err(map_admission_failure_to_api_error(failure));
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if active_instruction_inputs(&loaded.state) == *desired {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

pub(super) fn active_instruction_inputs(
    state: &engine::CoreAgentState,
) -> BTreeMap<ContextEntryKey, ContextEntryInput> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::Instructions))
        .filter_map(|entry| {
            let key = entry.key.clone()?;
            if !context_key_is_in_prefix(&key, INSTRUCTIONS_CONTEXT_PREFIX) {
                return None;
            }
            Some((key, active_entry_input(entry)))
        })
        .collect()
}

fn context_key_is_in_prefix(key: &ContextEntryKey, prefix: &str) -> bool {
    key.as_str() == prefix
        || key
            .as_str()
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn replace_managed_instruction_source(
    mut active: BTreeMap<ContextEntryKey, ContextEntryInput>,
    owned_prefix: &str,
    source_entries: BTreeMap<ContextEntryKey, ContextEntryInput>,
    default_entry: ContextEntryInput,
) -> Result<BTreeMap<ContextEntryKey, ContextEntryInput>, AgentApiError> {
    active.retain(|key, _| !context_key_is_in_prefix(key, owned_prefix));
    for (key, entry) in source_entries {
        if !context_key_is_in_prefix(&key, owned_prefix) {
            return Err(AgentApiError::internal(format!(
                "managed instruction key {key} is outside owned prefix {owned_prefix}"
            )));
        }
        active.insert(key, entry);
    }

    active.remove(&ContextEntryKey::new(DEFAULT_INSTRUCTIONS_CONTEXT_KEY));
    if active.is_empty() {
        active.insert(
            ContextEntryKey::new(DEFAULT_INSTRUCTIONS_CONTEXT_KEY),
            default_entry,
        );
    }
    Ok(active)
}

fn default_instruction_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Instructions,
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(bytes: &[u8]) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Instructions,
            content_ref: BlobRef::from_bytes(bytes),
            media_type: Some("text/plain".to_owned()),
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    #[test]
    fn managed_sources_replace_only_their_subset_and_control_fallback() {
        let default = instruction(b"default");
        let profile = instruction(b"profile");
        let prompt = instruction(b"prompt");
        let default_key = ContextEntryKey::new(DEFAULT_INSTRUCTIONS_CONTEXT_KEY);
        let profile_key = ContextEntryKey::new("instructions.050.profile");
        let prompt_key = ContextEntryKey::new("instructions.100.prompts.0000.project");

        let active = BTreeMap::from([(default_key.clone(), default.clone())]);
        let with_profile = replace_managed_instruction_source(
            active,
            "instructions.050.profile",
            BTreeMap::from([(profile_key.clone(), profile.clone())]),
            default.clone(),
        )
        .expect("apply profile");
        assert_eq!(
            with_profile,
            BTreeMap::from([(profile_key.clone(), profile)])
        );

        let with_both = replace_managed_instruction_source(
            with_profile,
            "instructions.100.prompts",
            BTreeMap::from([(prompt_key.clone(), prompt.clone())]),
            default.clone(),
        )
        .expect("apply prompts");
        assert_eq!(with_both.len(), 2);
        assert!(with_both.contains_key(&profile_key));
        assert!(with_both.contains_key(&prompt_key));
        assert!(!with_both.contains_key(&default_key));

        let prompts_only = replace_managed_instruction_source(
            with_both,
            "instructions.050.profile",
            BTreeMap::new(),
            default.clone(),
        )
        .expect("clear profile");
        assert_eq!(prompts_only, BTreeMap::from([(prompt_key, prompt)]));

        let fallback = replace_managed_instruction_source(
            prompts_only,
            "instructions.100.prompts",
            BTreeMap::new(),
            default.clone(),
        )
        .expect("clear prompts");
        assert_eq!(fallback, BTreeMap::from([(default_key, default)]));
    }
}
