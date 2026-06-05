use super::*;

impl GatewayAgentApi {
    pub(super) async fn load_session_state_with_current_skill_catalog(
        &self,
        session_id: &SessionId,
    ) -> Result<LoadedSession, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        if loaded.state.lifecycle.status == CoreAgentStatus::Open
            && loaded.state.runs.active.is_none()
            && loaded.state.runs.queued.is_empty()
        {
            self.refresh_skill_catalog_for_idle_session(
                session_id,
                active_skill_catalog_ref(&loaded.state),
            )
            .await?;
            return self.load_session_state(session_id).await;
        }
        Ok(loaded)
    }

    pub(super) async fn refresh_skill_catalog_for_idle_session(
        &self,
        session_id: &SessionId,
        active_catalog_ref: Option<BlobRef>,
    ) -> Result<(), AgentApiError> {
        let Some(command) = self
            .skill_catalog_refresh_command(session_id, active_catalog_ref)
            .await?
        else {
            return Ok(());
        };
        let target_catalog_ref = match &command {
            CoreAgentCommand::UpsertContext { key, entry }
                if key.as_str() == SKILL_CATALOG_CONTEXT_KEY
                    && matches!(entry.kind, ContextEntryKind::SkillCatalog) =>
            {
                Some(entry.content_ref.clone())
            }
            CoreAgentCommand::RemoveContext { key }
                if key.as_str() == SKILL_CATALOG_CONTEXT_KEY =>
            {
                None
            }
            _ => {
                return Err(AgentApiError::internal(
                    "skill catalog refresh produced non-catalog context command",
                ));
            }
        };
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(session_id, command).await?;
        self.wait_for_skill_catalog(session_id, target_catalog_ref, baseline_failures)
            .await
    }

    pub(super) async fn skill_catalog_refresh_command(
        &self,
        session_id: &SessionId,
        active_catalog_ref: Option<BlobRef>,
    ) -> Result<Option<CoreAgentCommand>, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let specs = conventional_vfs_skill_root_specs(&mounts);
        if specs.is_empty() {
            return Ok(clear_skill_catalog_command(active_catalog_ref.as_ref()));
        }

        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let resolved = resolve_mounted_vfs_skill_roots(blobs, workspace_store, mounts, specs)
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        if inputs.is_empty() {
            return Ok(clear_skill_catalog_command(active_catalog_ref.as_ref()));
        }

        let mut state = engine::CoreAgentState::new();
        if let Some(catalog_ref) = active_catalog_ref {
            state.context.entries = vec![active_catalog_entry(catalog_ref)];
        }
        let publication =
            prepare_skill_catalog_publication(self.store.as_ref(), &state, None, &inputs)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
        Ok(publication.command)
    }

    pub(super) async fn project_skill_list(
        &self,
        loaded: &LoadedSession,
    ) -> Result<SkillListResponse, AgentApiError> {
        let Some(catalog_ref) = active_skill_catalog_ref(&loaded.state) else {
            return Ok(SkillListResponse {
                catalog_ref: None,
                skills: Vec::new(),
            });
        };
        let catalog = self.read_skill_catalog(&catalog_ref).await?;
        Ok(skill_list_response(
            Some(&catalog_ref),
            Some(&catalog),
            &active_skill_context_entries(&loaded.state),
        ))
    }

    pub(super) async fn project_active_skills(
        &self,
        loaded: &LoadedSession,
    ) -> Result<SkillActiveResponse, AgentApiError> {
        let catalog_ref = active_skill_catalog_ref(&loaded.state);
        let catalog = match catalog_ref.as_ref() {
            Some(catalog_ref) => Some(self.read_skill_catalog(catalog_ref).await?),
            None => None,
        };
        Ok(skill_active_response(
            catalog_ref.as_ref(),
            catalog.as_ref(),
            &active_skill_context_entries(&loaded.state),
        ))
    }

    pub(super) async fn read_skill_catalog(
        &self,
        catalog_ref: &BlobRef,
    ) -> Result<SkillCatalogSnapshot, AgentApiError> {
        let bytes = self
            .store
            .read_bytes(catalog_ref)
            .await
            .map_err(map_blob_read_error)?;
        serde_json::from_slice(&bytes).map_err(|error| {
            AgentApiError::internal(format!("stored skill catalog is invalid JSON: {error}"))
        })
    }

    pub(super) async fn read_skill_doc_for_activation(
        &self,
        session_id: &SessionId,
        skill: &SkillMetadata,
    ) -> Result<String, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        read_skill_doc_for_activation_from_vfs(blobs, workspace_store, mounts, skill).await
    }

    pub(super) fn require_open_idle_session(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
        operation: &str,
    ) -> Result<(), AgentApiError> {
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(format!(
                "{operation} can only change while no run is active or queued"
            )));
        }
        Ok(())
    }

    pub(super) async fn wait_for_skill_catalog(
        &self,
        session_id: &SessionId,
        target_catalog_ref: Option<BlobRef>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for skill catalog update: {session_id}"
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
            let actual = active_skill_catalog_ref(&loaded.state);
            if actual == target_catalog_ref {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_skill_activations(
        &self,
        session_id: &SessionId,
        target: Vec<SkillId>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for skill activation update: {session_id}"
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
            if active_skill_ids(&loaded.state) == target {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

pub(super) fn clear_skill_catalog_command(
    active_catalog_ref: Option<&BlobRef>,
) -> Option<CoreAgentCommand> {
    active_catalog_ref.map(|_| CoreAgentCommand::RemoveContext {
        key: ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY),
    })
}

pub(super) fn active_catalog_entry(catalog_ref: BlobRef) -> ContextEntry {
    let input = skill_catalog_context_input(catalog_ref);
    ContextEntry {
        entry_id: engine::ContextEntryId::new(1),
        key: Some(ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY)),
        kind: ContextEntryKind::SkillCatalog,
        source: engine::ContextEntrySource::Runtime {
            label: "skills.catalog".to_owned(),
        },
        content_ref: input.content_ref,
        media_type: input.media_type,
        preview: input.preview,
        provider_kind: input.provider_kind,
        provider_item_id: input.provider_item_id,
        token_estimate: input.token_estimate,
    }
}

pub(super) fn active_skill_catalog_ref(state: &engine::CoreAgentState) -> Option<BlobRef> {
    state
        .context
        .entries
        .iter()
        .find(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|key| key.as_str() == SKILL_CATALOG_CONTEXT_KEY)
                && matches!(entry.kind, ContextEntryKind::SkillCatalog)
        })
        .map(|entry| entry.content_ref.clone())
}

pub(super) fn active_skill_context_entries(state: &engine::CoreAgentState) -> Vec<&ContextEntry> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::SkillActivation { .. }))
        .collect()
}

pub(super) fn active_skill_ids(state: &engine::CoreAgentState) -> Vec<SkillId> {
    active_skill_context_entries(state)
        .into_iter()
        .filter_map(|entry| match &entry.kind {
            ContextEntryKind::SkillActivation { skill_id } => Some(skill_id.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn active_skill_ids_after_upsert(
    state: &engine::CoreAgentState,
    skill_id: SkillId,
) -> Vec<SkillId> {
    let mut ids = active_skill_ids(state);
    ids.retain(|active| active != &skill_id);
    ids.push(skill_id);
    ids
}

pub(super) fn active_skill_ids_after_remove(
    state: &engine::CoreAgentState,
    skill_id: &SkillId,
) -> Vec<SkillId> {
    let mut ids = active_skill_ids(state);
    ids.retain(|active| active != skill_id);
    ids
}

pub(super) fn skill_activation_context_input(
    skill_id: SkillId,
    catalog_ref: BlobRef,
    context_ref: BlobRef,
    scope: ApiSkillActivationScope,
    skill: Option<&SkillMetadata>,
) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::SkillActivation { skill_id },
        content_ref: context_ref,
        media_type: Some("text/markdown".to_owned()),
        preview: skill.map(|skill| format!("skill activated: {}", skill.name)),
        provider_kind: Some(skill_activation_provider_kind(scope).to_owned()),
        provider_item_id: Some(catalog_ref.as_str().to_owned()),
        token_estimate: None,
    }
}

pub(super) fn skill_activation_provider_kind(scope: ApiSkillActivationScope) -> &'static str {
    match scope {
        ApiSkillActivationScope::Run => SKILL_ACTIVATION_PROVIDER_KIND_RUN,
        ApiSkillActivationScope::Session => SKILL_ACTIVATION_PROVIDER_KIND_SESSION,
    }
}

pub(super) fn skill_list_response(
    catalog_ref: Option<&BlobRef>,
    catalog: Option<&SkillCatalogSnapshot>,
    activations: &[&ContextEntry],
) -> SkillListResponse {
    let Some(catalog) = catalog else {
        return SkillListResponse {
            catalog_ref: None,
            skills: Vec::new(),
        };
    };
    let active_ids = activations
        .iter()
        .filter_map(|entry| match &entry.kind {
            ContextEntryKind::SkillActivation { skill_id } => Some(skill_id.as_str().to_owned()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    SkillListResponse {
        catalog_ref: catalog_ref.map(|catalog_ref| catalog_ref.as_str().to_owned()),
        skills: catalog
            .skills
            .iter()
            .map(|skill| SkillListItem {
                skill_id: skill.skill_id.as_str().to_owned(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                enabled: skill.enabled,
                active: active_ids.contains(skill.skill_id.as_str()),
            })
            .collect(),
    }
}

pub(super) fn skill_active_response(
    catalog_ref: Option<&BlobRef>,
    catalog: Option<&SkillCatalogSnapshot>,
    activations: &[&ContextEntry],
) -> SkillActiveResponse {
    SkillActiveResponse {
        catalog_ref: catalog_ref.map(|catalog_ref| catalog_ref.as_str().to_owned()),
        activations: activations
            .iter()
            .filter_map(|activation| skill_activation_view(activation, catalog_ref, catalog))
            .collect(),
    }
}

pub(super) async fn read_skill_doc_for_activation_from_vfs(
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mounts: Vec<VfsMountRecord>,
    skill: &SkillMetadata,
) -> Result<String, AgentApiError> {
    let skill_doc_path = match &skill.location {
        SkillLocation::MountedSnapshot { skill_doc_path, .. }
        | SkillLocation::MountedWorkspace { skill_doc_path, .. } => skill_doc_path,
        SkillLocation::HostFilesystem { .. } => {
            return Err(AgentApiError::invalid_request(
                "direct skill activation currently supports VFS-mounted skills only",
            ));
        }
    };

    let fs = MountedVfsFileSystem::new(blobs, workspace_store, mounts).map_err(map_fs_error)?;
    let path = FsPath::new(skill_doc_path.as_str()).map_err(|error| {
        AgentApiError::internal(format!(
            "stored skill document path is invalid: {skill_doc_path}: {error}"
        ))
    })?;
    fs.read_file_text(&path).await.map_err(map_fs_error)
}

pub(super) fn api_skill_activation_scope(entry: &ContextEntry) -> ApiSkillActivationScope {
    match entry.provider_kind.as_deref() {
        Some(SKILL_ACTIVATION_PROVIDER_KIND_RUN) => ApiSkillActivationScope::Run,
        _ => ApiSkillActivationScope::Session,
    }
}

pub(super) fn skill_activation_view(
    activation: &ContextEntry,
    active_catalog_ref: Option<&BlobRef>,
    catalog: Option<&SkillCatalogSnapshot>,
) -> Option<SkillActivationView> {
    let ContextEntryKind::SkillActivation { skill_id } = &activation.kind else {
        return None;
    };
    let metadata = catalog.and_then(|catalog| {
        catalog
            .skills
            .iter()
            .find(|skill| &skill.skill_id == skill_id)
    });
    let catalog_ref = activation
        .provider_item_id
        .as_deref()
        .or_else(|| active_catalog_ref.map(BlobRef::as_str))?;
    Some(SkillActivationView {
        skill_id: skill_id.as_str().to_owned(),
        name: metadata.map(|skill| skill.name.clone()),
        description: metadata.map(|skill| skill.description.clone()),
        short_description: metadata.and_then(|skill| skill.short_description.clone()),
        catalog_ref: catalog_ref.to_owned(),
        scope: api_skill_activation_scope(activation),
        source: ApiSkillActivationSource::DirectContext {
            context_ref: activation.content_ref.as_str().to_owned(),
        },
    })
}
