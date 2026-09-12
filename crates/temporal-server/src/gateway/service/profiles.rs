use super::api_config::engine_session_config_from_api;
use super::*;
use ::profiles::{ProfileError, ProfileSourceExt, ProfileStore};

pub(super) fn merge_profile_start_metadata(
    profile_metadata: Option<&BTreeMap<String, String>>,
    explicit_metadata: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut metadata = profile_metadata.cloned().unwrap_or_default();
    metadata.extend(explicit_metadata);
    metadata
}

pub(super) fn merge_profile_start_retention(
    profile_default: Option<u64>,
    explicit: Option<Option<u64>>,
) -> Option<u64> {
    explicit.unwrap_or(profile_default)
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
        // Bots pin the profile by id: every open bot on it re-applies the
        // new revision at its controller's next idle boundary.
        self.signal_bots_for_profile(&profile.profile_id).await;
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
        let profile = self.profile_intent(&resolved, true)?;
        let applied = self
            .prepare_session_operation(
                &session_id,
                temporal_workflow::SessionOperation::ApplyProfile {
                    profile,
                    expected_config_revision: params.expected_config_revision,
                    expected_tools_revision: params.expected_tools_revision,
                },
            )
            .await?;
        let session = self.project_session_by_id(&session_id).await?;
        Ok(ProfileApplyResponse { session, applied })
    }

    pub(super) async fn resolve_profile_source(
        &self,
        source: ProfileSource,
    ) -> Result<ProfileDocument, AgentApiError> {
        source.validate().map_err(map_profile_error)?;
        match source {
            ProfileSource::Named { profile_id } => {
                let profile = self
                    .store
                    .read_agent_profile(&profile_id)
                    .await
                    .map_err(map_profile_error)?;
                Ok(profile.document)
            }
            ProfileSource::Inline { profile } => Ok(profile.document),
        }
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

    pub(super) fn profile_intent(
        &self,
        profile: &ProfileDocument,
        apply_config: bool,
    ) -> Result<temporal_workflow::SessionProfileIntent, AgentApiError> {
        Ok(temporal_workflow::SessionProfileIntent {
            config: if apply_config {
                profile
                    .config
                    .clone()
                    .map(|config| {
                        engine_session_config_from_api(config, self.default_model.clone())
                    })
                    .transpose()?
            } else {
                None
            },
            instructions: profile.instructions.clone(),
            environment: profile.environment.clone(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_start_metadata_overrides_profile_defaults() {
        let profile = BTreeMap::from([
            ("campaign".to_owned(), "profile-campaign".to_owned()),
            ("owner".to_owned(), "evaluation".to_owned()),
        ]);
        let explicit = BTreeMap::from([
            ("campaign".to_owned(), "request-campaign".to_owned()),
            ("trial".to_owned(), "42".to_owned()),
        ]);

        assert_eq!(
            merge_profile_start_metadata(Some(&profile), explicit),
            BTreeMap::from([
                ("campaign".to_owned(), "request-campaign".to_owned()),
                ("owner".to_owned(), "evaluation".to_owned()),
                ("trial".to_owned(), "42".to_owned()),
            ])
        );
    }

    #[test]
    fn explicit_start_retention_overrides_or_clears_profile_default() {
        assert_eq!(merge_profile_start_retention(Some(100), None), Some(100));
        assert_eq!(merge_profile_start_retention(Some(100), Some(None)), None);
        assert_eq!(
            merge_profile_start_retention(Some(100), Some(Some(200))),
            Some(200)
        );
    }
}
