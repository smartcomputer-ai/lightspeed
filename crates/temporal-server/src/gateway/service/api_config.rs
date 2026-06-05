use super::*;

impl GatewayAgentApi {
    pub(super) async fn session_config_for_start(
        &self,
        api_config: Option<SessionConfigInput>,
    ) -> Result<SessionConfig, AgentApiError> {
        let mut config = default_session_config(self.default_model.clone());
        self.apply_session_config_input(&mut config, api_config)
            .await?;
        config
            .validate_provider_compatibility()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        Ok(config)
    }

    pub(super) async fn apply_session_config_input(
        &self,
        config: &mut SessionConfig,
        api_config: Option<SessionConfigInput>,
    ) -> Result<(), AgentApiError> {
        let Some(api_config) = api_config else {
            return Ok(());
        };
        if let Some(model) = api_config.model {
            let previous_api_kind = config.model.api_kind.clone();
            config.model = model_selection_from_api(model)?;
            if config.model.api_kind != previous_api_kind {
                config.turn.provider_request_defaults =
                    default_provider_request_defaults(&config.model.api_kind);
            }
        }
        apply_generation_config(config, api_config.generation)?;
        apply_context_config(&mut config.context, api_config.context);
        apply_run_defaults_config(&mut config.run, api_config.run_defaults);
        Ok(())
    }

    pub(super) async fn core_session_patch_from_api(
        &self,
        current: &SessionConfig,
        patch: SessionConfigPatchInput,
    ) -> Result<SessionConfigPatch, AgentApiError> {
        let model = patch.model.map(model_selection_from_api).transpose()?;
        let turn = turn_config_patch_from_api(current, patch.generation)?;
        Ok(SessionConfigPatch {
            model,
            run: run_config_patch_from_api(patch.run_defaults),
            turn,
            context: context_config_patch_from_api(patch.context),
        })
    }

    pub(super) async fn run_config_for_start(
        &self,
        session_id: &SessionId,
        api_config: Option<RunStartConfig>,
    ) -> Result<RunConfig, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        let session_config = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is not open: {session_id}"))
        })?;
        let mut run_config = session_config.run.clone();
        apply_run_start_config(&mut run_config, session_config, api_config)?;
        Ok(run_config)
    }
}

pub(super) fn apply_generation_config(
    config: &mut SessionConfig,
    generation: Option<GenerationConfig>,
) -> Result<(), AgentApiError> {
    let Some(generation) = generation else {
        return Ok(());
    };
    if let Some(max_output_tokens) = generation.max_output_tokens {
        config.turn.max_output_tokens = Some(max_output_tokens);
    }
    if let Some(effort) = generation.reasoning_effort {
        config.turn.provider_request_defaults = provider_defaults_with_reasoning(
            &config.model.api_kind,
            &config.turn.provider_request_defaults,
            effort,
        )?;
    }
    Ok(())
}

pub(super) fn apply_context_config(
    config: &mut engine::ContextConfig,
    context: Option<ApiContextConfigInput>,
) {
    let Some(context) = context else {
        return;
    };
    if let Some(compaction) = context.compaction {
        config.compaction = Some(compaction_policy_from_api(compaction));
    }
}

pub(super) fn apply_run_defaults_config(
    config: &mut RunConfig,
    run_defaults: Option<RunDefaultsConfig>,
) {
    let Some(run_defaults) = run_defaults else {
        return;
    };
    if let Some(max_turns) = run_defaults.max_turns {
        config.max_turns = Some(max_turns);
    }
    if let Some(max_tool_rounds) = run_defaults.max_tool_rounds {
        config.max_tool_rounds = Some(max_tool_rounds);
    }
}

pub(super) fn apply_run_start_config(
    run_config: &mut RunConfig,
    session_config: &SessionConfig,
    api_config: Option<RunStartConfig>,
) -> Result<(), AgentApiError> {
    let Some(api_config) = api_config else {
        return Ok(());
    };
    let effective_api_kind = if let Some(model) = api_config.model {
        let model = model_selection_from_api(model)?;
        let api_kind = model.api_kind.clone();
        run_config.model_override = Some(model);
        api_kind
    } else {
        session_config.model.api_kind.clone()
    };
    if let Some(generation) = api_config.generation {
        if let Some(max_output_tokens) = generation.max_output_tokens {
            run_config.max_output_tokens = Some(max_output_tokens);
        }
        if let Some(effort) = generation.reasoning_effort {
            run_config.provider_request_defaults = Some(provider_defaults_with_reasoning(
                &effective_api_kind,
                &session_config.turn.provider_request_defaults,
                effort,
            )?);
        }
    }
    if let Some(limits) = api_config.limits {
        apply_run_limits_config(run_config, limits);
    }
    run_config
        .validate_provider_compatibility(&session_config.model.api_kind)
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))
}

pub(super) fn apply_run_limits_config(run_config: &mut RunConfig, limits: RunLimitsConfig) {
    if let Some(max_turns) = limits.max_turns {
        run_config.max_turns = Some(max_turns);
    }
    if let Some(max_tool_rounds) = limits.max_tool_rounds {
        run_config.max_tool_rounds = Some(max_tool_rounds);
    }
}

pub(super) fn run_config_patch_from_api(patch: Option<RunDefaultsPatch>) -> RunConfigPatch {
    let Some(patch) = patch else {
        return RunConfigPatch::default();
    };
    RunConfigPatch {
        max_turns: patch.max_turns.map(optional_patch_from_api),
        max_tool_rounds: patch.max_tool_rounds.map(optional_patch_from_api),
        ..RunConfigPatch::default()
    }
}

pub(super) fn turn_config_patch_from_api(
    current: &SessionConfig,
    patch: Option<GenerationConfigPatch>,
) -> Result<TurnConfigPatch, AgentApiError> {
    let Some(patch) = patch else {
        return Ok(TurnConfigPatch::default());
    };
    let provider_request_defaults = patch
        .reasoning_effort
        .map(|effort| {
            provider_defaults_with_reasoning(
                &current.model.api_kind,
                &current.turn.provider_request_defaults,
                effort,
            )
        })
        .transpose()?;
    Ok(TurnConfigPatch {
        max_output_tokens: patch.max_output_tokens.map(optional_patch_from_api),
        provider_request_defaults,
    })
}

pub(super) fn context_config_patch_from_api(
    patch: Option<ContextConfigPatchInput>,
) -> ContextConfigPatch {
    let Some(patch) = patch else {
        return ContextConfigPatch::default();
    };
    ContextConfigPatch {
        compaction: patch
            .compaction
            .map(|patch| optional_patch_from_api_map(patch, compaction_policy_from_api)),
    }
}

pub(super) fn optional_patch_from_api<T>(patch: FieldPatch<T>) -> OptionalConfigPatch<T> {
    match patch {
        FieldPatch::Set(value) => OptionalConfigPatch::Set(value),
        FieldPatch::Clear => OptionalConfigPatch::Clear,
    }
}

pub(super) fn optional_patch_from_api_map<T, U>(
    patch: FieldPatch<T>,
    map: impl FnOnce(T) -> U,
) -> OptionalConfigPatch<U> {
    match patch {
        FieldPatch::Set(value) => OptionalConfigPatch::Set(map(value)),
        FieldPatch::Clear => OptionalConfigPatch::Clear,
    }
}

pub(super) fn compaction_policy_from_api(policy: CompactionPolicyInput) -> CompactionPolicy {
    match policy {
        CompactionPolicyInput::Disabled => CompactionPolicy::Disabled,
        CompactionPolicyInput::ProviderTriggered {
            compact_threshold_tokens,
        } => CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        },
        CompactionPolicyInput::ProviderStandalone {
            compact_threshold_tokens,
            target_tokens,
        } => CompactionPolicy::ProviderStandalone {
            compact_threshold_tokens,
            target_tokens,
        },
    }
}

pub(super) fn model_selection_from_api(
    model: ModelConfig,
) -> Result<ModelSelection, AgentApiError> {
    Ok(ModelSelection {
        api_kind: api_kind_from_str(&model.api_kind)?,
        provider_id: model.provider_id,
        model: model.model,
        options: ModelProviderOptions::None,
    })
}

pub(super) fn default_provider_request_defaults(
    api_kind: &ProviderApiKind,
) -> ProviderRequestDefaults {
    match api_kind {
        ProviderApiKind::OpenAiResponses => {
            ProviderRequestDefaults::OpenAiResponses(OpenAiResponsesRequestDefaults::default())
        }
        ProviderApiKind::AnthropicMessages => {
            ProviderRequestDefaults::AnthropicMessages(AnthropicMessagesRequestDefaults::default())
        }
        ProviderApiKind::OpenAiCompletions => {
            ProviderRequestDefaults::OpenAiCompletions(OpenAiCompletionsRequestDefaults::default())
        }
    }
}

pub(super) fn provider_defaults_with_reasoning(
    api_kind: &ProviderApiKind,
    base: &ProviderRequestDefaults,
    effort: ReasoningEffort,
) -> Result<ProviderRequestDefaults, AgentApiError> {
    if api_kind != &ProviderApiKind::OpenAiResponses {
        return Err(AgentApiError::invalid_request(
            "reasoning effort is only supported for openai:responses",
        ));
    }
    let mut defaults = match base {
        ProviderRequestDefaults::OpenAiResponses(defaults) => defaults.clone(),
        ProviderRequestDefaults::None => OpenAiResponsesRequestDefaults::default(),
        other => {
            return Err(AgentApiError::invalid_request(format!(
                "request defaults {other:?} do not match openai:responses"
            )));
        }
    };
    defaults.reasoning = match effort {
        ReasoningEffort::None => None,
        ReasoningEffort::Low => Some(openai_reasoning("low")),
        ReasoningEffort::Medium => Some(openai_reasoning("medium")),
        ReasoningEffort::High => Some(openai_reasoning("high")),
    };
    Ok(ProviderRequestDefaults::OpenAiResponses(defaults))
}

pub(super) fn openai_reasoning(effort: &str) -> OpenAiReasoningConfig {
    OpenAiReasoningConfig {
        effort: Some(effort.to_owned()),
        summary: Some("auto".to_owned()),
        extra: BTreeMap::new(),
    }
}
