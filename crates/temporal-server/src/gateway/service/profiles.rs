use super::api_config::engine_session_config_from_api;
use super::*;
use ::profiles::{ProfileError, ProfileSourceExt, ProfileStore};

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

    pub(super) async fn put_profile_record(
        &self,
        params: ProfilePutParams,
    ) -> Result<ProfilePutResponse, AgentApiError> {
        let profile = self
            .store
            .put_agent_profile(params.profile, params.expected_revision, now_ms()?)
            .await
            .map_err(map_profile_error)?;
        Ok(ProfilePutResponse { profile })
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

        applied.instructions_changed = self
            .apply_profile_instructions(session_id, document.instructions.clone())
            .await?;

        for mount in &document.mounts {
            if self.apply_profile_mount(session_id, mount.clone()).await? {
                applied.mounts_changed = applied.mounts_changed.saturating_add(1);
            }
        }

        if expected_tools_revision.is_some() {
            self.assert_tools_revision(session_id, expected_tools_revision)
                .await?;
        }

        for environment in &document.environments {
            if self
                .apply_profile_environment(session_id, environment.clone())
                .await?
            {
                applied.environments_changed = applied.environments_changed.saturating_add(1);
            }
        }

        self.load_session_state_with_current_run_context(session_id)
            .await?;
        let session = self.project_session_by_id(session_id).await?;
        Ok((session, applied))
    }

    pub(super) fn merge_profile_start_config(
        &self,
        profile_config: Option<api::SessionConfig>,
        explicit_config: Option<api::SessionConfig>,
    ) -> Option<api::SessionConfig> {
        let Some(profile_config) = profile_config else {
            return explicit_config;
        };
        let Some(explicit_config) = explicit_config else {
            return Some(profile_config);
        };
        Some(api::SessionConfig {
            model: explicit_config.model.or(profile_config.model),
            generation: explicit_config.generation.or(profile_config.generation),
            limits: explicit_config.limits.or(profile_config.limits),
            context: explicit_config.context.or(profile_config.context),
            features: explicit_config.features.or(profile_config.features),
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
        config: api::SessionConfig,
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
        // Apply means "make the session's config the profile's config":
        // full-document put semantics, sections absent from the profile
        // revert to defaults.
        let candidate = engine_session_config_from_api(config.clone(), self.default_model.clone())?;
        candidate
            .validate()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        if &candidate == current {
            return Ok(false);
        }
        self.put_session_config(SessionConfigPutParams {
            session_id: session_id.as_str().to_owned(),
            expected_config_revision: Some(loaded.state.lifecycle.config_revision),
            config,
        })
        .await?;
        Ok(true)
    }

    async fn apply_profile_instructions(
        &self,
        session_id: &SessionId,
        instructions: Option<ProfileInstructions>,
    ) -> Result<bool, AgentApiError> {
        let mut source_entries = BTreeMap::new();
        if let Some(instructions) = instructions {
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
            source_entries.insert(
                ContextEntryKey::new(PROFILE_INSTRUCTIONS_CONTEXT_KEY),
                ContextEntryInput {
                    kind: ContextEntryKind::Instructions,
                    content_ref,
                    media_type: Some("text/plain".to_owned()),
                    preview: Some("Profile instructions".to_owned()),
                    provider_kind: None,
                    provider_item_id: None,
                    token_estimate: None,
                },
            );
        }
        let loaded = self.load_session_state(session_id).await?;
        self.require_open_idle_session(session_id, &loaded, "profile instructions apply")?;
        self.reconcile_managed_instructions(
            session_id,
            &loaded.state,
            PROFILE_INSTRUCTIONS_CONTEXT_KEY,
            source_entries,
        )
        .await
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
        let instance_id = match environment.environment {
            api::ProfileEnvironmentSource::Existing { instance_id } => instance_id,
            api::ProfileEnvironmentSource::Provision {
                provider_id,
                request,
            } => {
                let allowed = loaded
                    .state
                    .lifecycle
                    .config
                    .as_ref()
                    .and_then(|config| config.features.environments.as_ref())
                    .is_some_and(|feature| {
                        feature.providers.as_ref().is_none_or(|providers| {
                            providers.iter().any(|candidate| candidate == &provider_id)
                        })
                    });
                if !allowed {
                    return Err(AgentApiError::rejected(format!(
                        "environment provider is not allowed by session config: {provider_id}"
                    )));
                }
                self.create_environment(EnvironmentCreateParams {
                    provider_id,
                    request,
                })
                .await?
                .result
                .environment
                .instance_id
            }
        };
        self.attach_session_environment(SessionEnvironmentAttachParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some(environment.env_id),
            instance_id,
            cwd: None,
            fs_routes: Vec::new(),
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
