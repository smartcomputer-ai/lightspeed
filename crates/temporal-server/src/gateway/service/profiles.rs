use super::*;
use ::profiles::{ProfileError, ProfileSourceExt, ProfileStore, UpdateAgentProfile};

const PROFILE_INSTRUCTIONS_CONTEXT_KEY: &str = "instructions.050.profile";

#[derive(Clone, Debug)]
pub(super) struct ResolvedAgentProfile {
    pub(super) document: ProfileDocument,
}

impl GatewayAgentApi {
    pub(super) async fn create_profile_record(
        &self,
        params: ProfileCreateParams,
    ) -> Result<ProfileCreateResponse, AgentApiError> {
        let created_at_ms = now_ms()?;
        let profile = self
            .store
            .create_agent_profile(params.profile, created_at_ms)
            .await
            .map_err(map_profile_error)?;
        Ok(ProfileCreateResponse { profile })
    }

    pub(super) async fn read_profile_record(
        &self,
        params: ProfileReadParams,
    ) -> Result<ProfileReadResponse, AgentApiError> {
        let profile = self
            .store
            .read_agent_profile(&params.profile_id)
            .await
            .map_err(map_profile_error)?;
        Ok(ProfileReadResponse { profile })
    }

    pub(super) async fn list_profile_records(
        &self,
        _params: ProfileListParams,
    ) -> Result<ProfileListResponse, AgentApiError> {
        let profiles = self
            .store
            .list_agent_profiles()
            .await
            .map_err(map_profile_error)?;
        Ok(ProfileListResponse { profiles })
    }

    pub(super) async fn update_profile_record(
        &self,
        params: ProfileUpdateParams,
    ) -> Result<ProfileUpdateResponse, AgentApiError> {
        if params.patch.is_empty() {
            let profile = self
                .store
                .read_agent_profile(&params.profile_id)
                .await
                .map_err(map_profile_error)?;
            if let Some(expected) = params.expected_revision
                && profile.revision != expected
            {
                return Err(AgentApiError::conflict(format!(
                    "expected profile revision {expected}, got {}",
                    profile.revision
                )));
            }
            return Ok(ProfileUpdateResponse { profile });
        }
        let profile = self
            .store
            .update_agent_profile(UpdateAgentProfile {
                profile_id: params.profile_id,
                expected_revision: params.expected_revision,
                patch: params.patch,
                updated_at_ms: now_ms()?,
            })
            .await
            .map_err(map_profile_error)?;
        Ok(ProfileUpdateResponse { profile })
    }

    pub(super) async fn delete_profile_record(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<ProfileDeleteResponse, AgentApiError> {
        let profile = self
            .store
            .delete_agent_profile(&params.profile_id)
            .await
            .map_err(map_profile_error)?;
        Ok(ProfileDeleteResponse { profile })
    }

    pub(super) async fn apply_profile_to_session(
        &self,
        params: ProfileApplyParams,
    ) -> Result<ProfileApplyResponse, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let resolved = self.resolve_profile_source(params.profile).await?;
        let (session, applied) = self
            .apply_profile_document(
                &session_id,
                &resolved.document,
                true,
                params.expected_config_revision,
                params.expected_tools_revision,
            )
            .await?;
        Ok(ProfileApplyResponse { session, applied })
    }

    pub(super) async fn resolve_profile_source(
        &self,
        source: ProfileSource,
    ) -> Result<ResolvedAgentProfile, AgentApiError> {
        source.validate().map_err(map_profile_error)?;
        match source {
            ProfileSource::Named { profile_id } => {
                let profile = self
                    .store
                    .read_agent_profile(&profile_id)
                    .await
                    .map_err(map_profile_error)?;
                Ok(ResolvedAgentProfile {
                    document: profile.document,
                })
            }
            ProfileSource::Inline { profile } => Ok(ResolvedAgentProfile {
                document: profile.document,
            }),
        }
    }

    pub(super) async fn apply_profile_document(
        &self,
        session_id: &SessionId,
        document: &ProfileDocument,
        apply_config: bool,
        expected_config_revision: Option<u64>,
        expected_tools_revision: Option<u64>,
    ) -> Result<(SessionView, ProfileApplySummary), AgentApiError> {
        let mut applied = ProfileApplySummary::default();

        if apply_config {
            if let Some(config) = document.config.clone() {
                applied.config_changed = self
                    .apply_profile_config(session_id, config, expected_config_revision)
                    .await?;
            } else if expected_config_revision.is_some() {
                self.assert_config_revision(session_id, expected_config_revision)
                    .await?;
            }
        }

        if document.instructions.is_some() {
            applied.instructions_changed = self
                .apply_profile_instructions(session_id, document.instructions.clone())
                .await?;
        }

        for mount in &document.mounts {
            if self.apply_profile_mount(session_id, mount.clone()).await? {
                applied.mounts_changed = applied.mounts_changed.saturating_add(1);
            }
        }

        if !document.mcp.is_empty() {
            self.assert_tools_revision(session_id, expected_tools_revision)
                .await?;
        } else if expected_tools_revision.is_some() {
            self.assert_tools_revision(session_id, expected_tools_revision)
                .await?;
        }
        for link in &document.mcp {
            if self
                .apply_profile_mcp_link(session_id, link.clone())
                .await?
            {
                applied.mcp_changed = applied.mcp_changed.saturating_add(1);
            }
        }

        for environment in &document.environments {
            if self
                .apply_profile_environment(session_id, environment.clone())
                .await?
            {
                applied.environments_changed = applied.environments_changed.saturating_add(1);
            }
        }

        let session = self.project_session_by_id(session_id).await?;
        Ok((session, applied))
    }

    pub(super) fn merge_profile_start_config(
        &self,
        profile_config: Option<SessionConfigInput>,
        explicit_config: Option<SessionConfigInput>,
    ) -> Option<SessionConfigInput> {
        let Some(profile_config) = profile_config else {
            return explicit_config;
        };
        let Some(explicit_config) = explicit_config else {
            return Some(profile_config);
        };
        Some(SessionConfigInput {
            model: explicit_config.model.or(profile_config.model),
            generation: explicit_config.generation.or(profile_config.generation),
            context: explicit_config.context.or(profile_config.context),
            run_defaults: explicit_config.run_defaults.or(profile_config.run_defaults),
            tools: explicit_config.tools.or(profile_config.tools),
        })
    }

    async fn assert_config_revision(
        &self,
        session_id: &SessionId,
        expected: Option<u64>,
    ) -> Result<(), AgentApiError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let loaded = self.load_session_state(session_id).await?;
        let actual = loaded.state.lifecycle.config_revision;
        if expected != actual {
            return Err(AgentApiError::conflict(format!(
                "expected config revision {expected}, got {actual}"
            )));
        }
        Ok(())
    }

    async fn assert_tools_revision(
        &self,
        session_id: &SessionId,
        expected: Option<u64>,
    ) -> Result<(), AgentApiError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let loaded = self.load_session_state(session_id).await?;
        let actual = loaded.state.tooling.revision;
        if expected != actual {
            return Err(AgentApiError::conflict(format!(
                "expected tools revision {expected}, got {actual}"
            )));
        }
        Ok(())
    }

    async fn apply_profile_config(
        &self,
        session_id: &SessionId,
        config: SessionConfigInput,
        expected_revision: Option<u64>,
    ) -> Result<bool, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        self.require_open_idle_session(session_id, &loaded, "profile config apply")?;
        let current = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is missing config: {session_id}"))
        })?;
        if let Some(expected) = expected_revision {
            let actual = loaded.state.lifecycle.config_revision;
            if expected != actual {
                return Err(AgentApiError::conflict(format!(
                    "expected config revision {expected}, got {actual}"
                )));
            }
        }
        let mut candidate = current.clone();
        self.apply_session_config_input(&mut candidate, Some(config.clone()))
            .await?;
        candidate
            .validate_provider_compatibility()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        if &candidate == current {
            return Ok(false);
        }
        self.update_session(SessionUpdateParams {
            session_id: session_id.as_str().to_owned(),
            expected_config_revision: Some(loaded.state.lifecycle.config_revision),
            patch: session_config_patch_from_input(config),
        })
        .await?;
        Ok(true)
    }

    async fn apply_profile_instructions(
        &self,
        session_id: &SessionId,
        instructions: Option<ProfileInstructions>,
    ) -> Result<bool, AgentApiError> {
        let Some(instructions) = instructions else {
            return Ok(false);
        };
        let content_ref = match instructions {
            ProfileInstructions::Text { text } => self
                .store
                .as_ref()
                .put_bytes(text.into_bytes())
                .await
                .map_err(map_blob_store_error)?,
            ProfileInstructions::TextRef { blob_ref } => {
                let blob_ref = parse_blob_ref(&blob_ref)?;
                if !self
                    .store
                    .as_ref()
                    .has_blob(&blob_ref)
                    .await
                    .map_err(map_blob_store_error)?
                {
                    return Err(AgentApiError::not_found(format!(
                        "profile instructions blob not found: {blob_ref}"
                    )));
                }
                blob_ref
            }
        };
        let key = ContextEntryKey::new(PROFILE_INSTRUCTIONS_CONTEXT_KEY);
        let entry = ContextEntryInput {
            kind: ContextEntryKind::Instructions,
            content_ref,
            media_type: Some("text/plain".to_owned()),
            preview: Some("Profile instructions".to_owned()),
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        };
        let loaded = self.load_session_state(session_id).await?;
        self.require_open_idle_session(session_id, &loaded, "profile instructions apply")?;
        let unchanged = loaded.state.context.entries.iter().any(|active| {
            active.key.as_ref() == Some(&key)
                && active.kind == entry.kind
                && active.content_ref == entry.content_ref
        });
        if unchanged {
            return Ok(false);
        }
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            session_id,
            CoreAgentCommand::UpsertContext {
                key: key.clone(),
                entry: entry.clone(),
            },
        )
        .await?;
        self.wait_for_context_entries_applied(session_id, &[(key, entry)], baseline_failures)
            .await?;
        Ok(true)
    }

    async fn apply_profile_mount(
        &self,
        session_id: &SessionId,
        mount: api::ProfileMount,
    ) -> Result<bool, AgentApiError> {
        let mount_path = VfsPath::parse(&mount.mount_path).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs mount path: {error}"))
        })?;
        let access = vfs_api::core_vfs_mount_access(mount.access);
        let source = self
            .validate_vfs_mount_source(mount.source.clone(), access)
            .await?;
        let existing = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?
            .into_iter()
            .find(|existing| existing.mount_path == mount_path);
        if existing
            .as_ref()
            .is_some_and(|existing| existing.source == source && existing.access == access)
        {
            return Ok(false);
        }
        self.put_vfs_mount_record(VfsMountPutParams {
            session_id: session_id.as_str().to_owned(),
            mount_path: mount.mount_path,
            source: mount.source,
            access: mount.access,
        })
        .await?;
        Ok(true)
    }

    async fn apply_profile_mcp_link(
        &self,
        session_id: &SessionId,
        link: api::ProfileMcpLink,
    ) -> Result<bool, AgentApiError> {
        let params = SessionMcpLinkParams {
            session_id: session_id.as_str().to_owned(),
            server_id: link.server_id,
            tool_id: link.tool_id,
            server_label: link.server_label,
            allowed_tools: link.allowed_tools,
            approval: link.approval,
            defer_loading: link.defer_loading,
            auth_grant_id: link.auth_grant_id,
        };
        let server_id = parse_mcp_server_id(params.server_id.clone())?;
        let server = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_error)?;
        let grant = match params.auth_grant_id.clone() {
            Some(grant_id) => {
                let grant_id = parse_auth_grant_id(grant_id)?;
                Some(
                    self.store
                        .read_grant(&grant_id)
                        .await
                        .map_err(map_auth_error)?,
                )
            }
            None => None,
        };
        let draft = session_mcp_link_from_record(params.clone(), &server, grant.as_ref())?;
        let tool_name = draft.tool_name.clone();
        let desired = engine::ToolSpec {
            name: tool_name.clone(),
            kind: engine::ToolKind::RemoteMcp(draft.spec),
            parallelism: engine::ToolParallelism::ParallelSafe,
            target_requirement: engine::ToolTargetRequirement::None,
        };
        let loaded = self.load_session_state(session_id).await?;
        self.require_open_idle_session(session_id, &loaded, "profile MCP apply")?;
        if loaded.state.tooling.tools.get(&tool_name) == Some(&desired) {
            return Ok(false);
        }
        self.link_session_mcp(params).await?;
        Ok(true)
    }

    async fn apply_profile_environment(
        &self,
        session_id: &SessionId,
        environment: api::ProfileEnvironment,
    ) -> Result<bool, AgentApiError> {
        let env_id = parse_environment_id(environment.env_id.clone())?;
        let loaded = self
            .load_session_state_with_current_environment_projection(session_id)
            .await?;
        let existing = self
            .project_session_environments(session_id, &loaded.state)
            .await?
            .environments
            .into_iter()
            .find(|candidate| candidate.env_id == env_id.as_str());
        if existing.is_some() {
            if environment.activate {
                let active_target = self
                    .activation_target_for_environment(session_id, &env_id)
                    .await?;
                if loaded
                    .state
                    .tooling
                    .routing
                    .default_targets
                    .get(tools::targets::ENV_TARGET_NAMESPACE)
                    != Some(&active_target)
                {
                    self.activate_session_environment(SessionEnvironmentActivateParams {
                        session_id: session_id.as_str().to_owned(),
                        env_id: env_id.as_str().to_owned(),
                    })
                    .await?;
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        self.attach_session_environment(SessionEnvironmentAttachParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some(environment.env_id),
            provider_id: environment.provider_id,
            request: HostTargetAttachRequestView::Target {
                target_id: environment.target_id,
            },
            activate: environment.activate,
        })
        .await?;
        Ok(true)
    }
}

pub(super) fn map_profile_error(error: ProfileError) -> AgentApiError {
    match error {
        ProfileError::AlreadyExists { profile_id } => {
            AgentApiError::conflict(format!("agent profile already exists: {profile_id}"))
        }
        ProfileError::NotFound { profile_id } => {
            AgentApiError::not_found(format!("agent profile not found: {profile_id}"))
        }
        ProfileError::RevisionConflict {
            profile_id,
            expected,
            actual,
        } => AgentApiError::conflict(format!(
            "agent profile revision conflict for {profile_id}: expected {expected}, got {actual}"
        )),
        ProfileError::InvalidInput { message } => AgentApiError::invalid_request(message),
        ProfileError::Store { message } => AgentApiError::internal(message),
    }
}

fn session_config_patch_from_input(input: SessionConfigInput) -> SessionConfigPatchInput {
    SessionConfigPatchInput {
        model: input.model,
        generation: input.generation.map(|generation| GenerationConfigPatch {
            max_output_tokens: generation.max_output_tokens.map(FieldPatch::Set),
            reasoning_effort: generation.reasoning_effort,
            tool_choice: generation.tool_choice.map(FieldPatch::Set),
        }),
        context: input.context.map(|context| ContextConfigPatchInput {
            compaction: context.compaction.map(FieldPatch::Set),
        }),
        run_defaults: input.run_defaults.map(|run_defaults| RunDefaultsPatch {
            max_turns: run_defaults.max_turns.map(FieldPatch::Set),
            max_tool_rounds: run_defaults.max_tool_rounds.map(FieldPatch::Set),
        }),
        tools: input.tools.map(|tools| ToolConfigPatchInput {
            web_search: tools.web_search.map(FieldPatch::Set),
            web_fetch: tools.web_fetch.map(FieldPatch::Set),
            filesystem: tools.filesystem.map(FieldPatch::Set),
            messaging: tools.messaging.map(FieldPatch::Set),
            fleet: tools.fleet.map(FieldPatch::Set),
        }),
    }
}
