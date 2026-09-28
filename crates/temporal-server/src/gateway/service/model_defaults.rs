use super::*;

pub(super) fn map_store_error(error: store_pg::ModelDefaultsStoreError) -> AgentApiError {
    match error {
        store_pg::ModelDefaultsStoreError::Conflict { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        store_pg::ModelDefaultsStoreError::Invalid(error) => error,
        other => AgentApiError::internal(other.to_string()),
    }
}

/// An explicit (already profile-merged) model never consults universe policy.
pub(super) async fn creation_model(
    explicit: Option<ModelConfig>,
    defaults: impl std::future::Future<Output = Result<api::ModelDefaults, AgentApiError>>,
) -> Result<ModelSelection, AgentApiError> {
    let model = match explicit {
        Some(model) => model,
        None => defaults
            .await?
            .agent_run
            .ok_or_else(|| AgentApiError::model_default_unset(api::ModelDefaultSlot::AgentRun))?,
    };
    api::ModelDefaultSlot::AgentRun.validate_model(&model)?;
    api_config::model_selection_from_api(model)
}

pub(super) fn current_session_model(
    state: &engine::CoreAgentState,
) -> Result<ModelSelection, AgentApiError> {
    state
        .lifecycle
        .config
        .as_ref()
        .map(|config| config.model.clone())
        .ok_or_else(|| AgentApiError::rejected("session has no configuration"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(provider: &str, kind: &str, model: &str) -> ModelConfig {
        ModelConfig {
            provider_id: provider.into(),
            api_kind: kind.into(),
            model: model.into(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_selection_does_not_read_universe_defaults() {
        let explicit = route("private", "openai:completions", "custom");
        let model = creation_model(Some(explicit), async {
            panic!("explicit model must not read defaults")
        })
        .await
        .unwrap();
        assert_eq!(model.provider_id, "private");
        assert_eq!(model.api_kind, ProviderApiKind::OpenAiCompletions);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn profile_merge_precedes_universe_resolution() {
        let profile = api::SessionConfig {
            model: Some(route("anthropic", "anthropic:messages", "profile")),
            ..Default::default()
        };
        for explicit in [None, Some(route("local", "openai:completions", "explicit"))] {
            let expected = explicit.clone().or(profile.model.clone()).unwrap();
            let merged = GatewayAgentApi::merge_profile_start_config(
                Some(profile.clone()),
                Some(api::SessionConfig {
                    model: explicit,
                    ..Default::default()
                }),
            )
            .unwrap();
            let model = creation_model(merged.model, async { panic!("merged selection must win") })
                .await
                .unwrap();
            assert_eq!(model.provider_id, expected.provider_id);
            assert_eq!(model.model, expected.model);
        }
    }

    #[test]
    fn applying_sparse_profiles_preserves_model_but_replaces_features() {
        let model =
            api_config::model_selection_from_api(route("local", "openai:completions", "pinned"))
                .unwrap();
        let mut previous =
            engine_session_config_from_api(api::SessionConfig::default(), model.clone()).unwrap();
        previous.features.web = Some(engine::WebFeature {
            version: 1,
            fetch: Some(engine::WebFetchFeature {}),
            search: None,
        });
        let profile = api::ProfileDocument {
            config: Some(api::SessionConfig::default()),
            ..Default::default()
        };
        let applied =
            GatewayAgentApi::profile_intent(&profile, Some(previous.model.clone())).unwrap();
        let config = applied.config.unwrap();
        assert_eq!(config.model, model);
        assert_eq!(config.features.web, None);
        let replaced =
            engine_session_config_from_api(api::SessionConfig::default(), previous.model).unwrap();
        assert_eq!(replaced, config);
        assert!(
            GatewayAgentApi::profile_intent(&profile, None)
                .unwrap()
                .config
                .is_none(),
            "creation already merged its profile configuration"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn universe_routes_are_independent_and_resolution_is_a_snapshot() {
        let mut first = api::ModelDefaults {
            revision: 1,
            agent_run: Some(route("anthropic", "anthropic:messages", "first")),
            speech_to_text: None,
        };
        let second = api::ModelDefaults {
            revision: 1,
            agent_run: Some(route("private", "openai:completions", "second")),
            speech_to_text: None,
        };
        let first_model = creation_model(None, async { Ok(first.clone()) })
            .await
            .unwrap();
        let second_model = creation_model(None, async { Ok(second) }).await.unwrap();
        let existing =
            engine_session_config_from_api(api::SessionConfig::default(), first_model).unwrap();
        first.agent_run = None;
        let error = creation_model(None, async { Ok(first) }).await.unwrap_err();
        assert_eq!(error.kind, AgentApiErrorKind::ModelDefaultUnset);
        assert_eq!(
            error.model_default_slot,
            Some(api::ModelDefaultSlot::AgentRun)
        );
        assert_eq!(existing.model.provider_id, "anthropic");
        assert_eq!(second_model.provider_id, "private");
        let replaced =
            engine_session_config_from_api(api::SessionConfig::default(), existing.model.clone())
                .unwrap();
        assert_eq!(replaced.model, existing.model);
    }
}
