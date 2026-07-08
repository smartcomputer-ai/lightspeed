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
                config.turn.provider_params = None;
            }
        }
        apply_generation_config(config, api_config.generation)?;
        apply_context_config(&mut config.context, api_config.context);
        apply_run_defaults_config(&mut config.run, api_config.run_defaults);
        apply_tool_config(&mut config.tools, api_config.tools);
        apply_fleet_config(&mut config.fleet, api_config.fleet);
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
            tools: tool_config_patch_from_api(patch.tools),
            fleet: fleet_config_patch_from_api(patch.fleet),
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
        config.turn.provider_params = Some(provider_params_with_reasoning(
            &config.model.api_kind,
            config.turn.provider_params.as_ref(),
            effort,
        )?);
    }
    if let Some(tool_choice) = generation.tool_choice {
        config.turn.tool_choice = Some(tool_choice_from_api(tool_choice)?);
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

pub(super) fn apply_tool_config(config: &mut engine::ToolConfig, tools: Option<ToolConfigInput>) {
    let Some(tools) = tools else {
        return;
    };
    if let Some(web_search) = tools.web_search {
        config.web_search = Some(web_search);
    }
    if let Some(web_fetch) = tools.web_fetch {
        config.web_fetch = Some(web_fetch);
    }
    if let Some(filesystem) = tools.filesystem {
        config.filesystem = Some(filesystem_tool_mode_from_api(filesystem));
    }
    if let Some(messaging) = tools.messaging {
        config.messaging = Some(messaging);
    }
    if let Some(fleet) = tools.fleet {
        config.fleet = Some(fleet);
    }
    if let Some(timer) = tools.timer {
        config.timer = Some(timer);
    }
}

pub(super) fn apply_fleet_config(
    config: &mut engine::FleetConfig,
    fleet: Option<FleetConfigInput>,
) {
    let Some(fleet) = fleet else {
        return;
    };
    if let Some(profiles) = fleet.profiles {
        config.profiles = fleet_profiles_config_from_api(profiles);
    }
    if let Some(spawn) = fleet.spawn {
        config.spawn = fleet_spawn_config_from_api(spawn);
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
            run_config.provider_params = Some(provider_params_with_reasoning(
                &effective_api_kind,
                session_config.turn.provider_params.as_ref(),
                effort,
            )?);
        }
        if let Some(tool_choice) = generation.tool_choice {
            run_config.tool_choice = Some(tool_choice_from_api(tool_choice)?);
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
    let provider_params = patch
        .reasoning_effort
        .map(|effort| {
            provider_params_with_reasoning(
                &current.model.api_kind,
                current.turn.provider_params.as_ref(),
                effort,
            )
            .map(OptionalConfigPatch::Set)
        })
        .transpose()?;
    Ok(TurnConfigPatch {
        max_output_tokens: patch.max_output_tokens.map(optional_patch_from_api),
        provider_params,
        tool_choice: patch
            .tool_choice
            .map(tool_choice_patch_from_api)
            .transpose()?,
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

pub(super) fn tool_config_patch_from_api(
    patch: Option<ToolConfigPatchInput>,
) -> engine::ToolConfigPatch {
    let Some(patch) = patch else {
        return engine::ToolConfigPatch::default();
    };
    engine::ToolConfigPatch {
        web_search: patch.web_search.map(optional_patch_from_api),
        web_fetch: patch.web_fetch.map(optional_patch_from_api),
        filesystem: patch
            .filesystem
            .map(|patch| optional_patch_from_api_map(patch, filesystem_tool_mode_from_api)),
        messaging: patch.messaging.map(optional_patch_from_api),
        fleet: patch.fleet.map(optional_patch_from_api),
        timer: patch.timer.map(optional_patch_from_api),
    }
}

pub(super) fn fleet_config_patch_from_api(
    patch: Option<FleetConfigPatchInput>,
) -> engine::FleetConfigPatch {
    let Some(patch) = patch else {
        return engine::FleetConfigPatch::default();
    };
    engine::FleetConfigPatch {
        profiles: patch
            .profiles
            .map(|patch| optional_patch_from_api_map(patch, fleet_profiles_config_from_api)),
        spawn: patch
            .spawn
            .map(|patch| optional_patch_from_api_map(patch, fleet_spawn_config_from_api)),
    }
}

fn fleet_profiles_config_from_api(
    profiles: FleetProfilesConfigInput,
) -> engine::FleetProfilesConfig {
    engine::FleetProfilesConfig {
        allow: profiles.allow.map(|allow| {
            allow
                .into_iter()
                .map(|profile_id| profile_id.as_str().to_owned())
                .collect()
        }),
        deny: profiles
            .deny
            .into_iter()
            .map(|profile_id| profile_id.as_str().to_owned())
            .collect(),
        inline: profiles.inline.unwrap_or(true),
    }
}

fn fleet_spawn_config_from_api(spawn: FleetSpawnConfigInput) -> engine::FleetSpawnConfig {
    engine::FleetSpawnConfig {
        bases: spawn.bases.map(|bases| {
            bases
                .into_iter()
                .map(|base| match base {
                    FleetSpawnBaseConfig::Self_ => engine::FleetSpawnBase::Self_,
                    FleetSpawnBaseConfig::Session => engine::FleetSpawnBase::Session,
                    FleetSpawnBaseConfig::Profile => engine::FleetSpawnBase::Profile,
                })
                .collect()
        }),
    }
}

fn filesystem_tool_mode_from_api(mode: api::FilesystemToolMode) -> engine::FilesystemToolMode {
    match mode {
        api::FilesystemToolMode::None => engine::FilesystemToolMode::None,
        api::FilesystemToolMode::ReadOnly => engine::FilesystemToolMode::ReadOnly,
        api::FilesystemToolMode::Edit => engine::FilesystemToolMode::Edit,
    }
}

fn tool_choice_from_api(choice: ToolChoiceConfig) -> Result<ToolChoice, AgentApiError> {
    Ok(ToolChoice {
        mode: match choice.mode {
            ToolChoiceModeConfig::Auto => ToolChoiceMode::Auto,
            ToolChoiceModeConfig::None => ToolChoiceMode::None,
            ToolChoiceModeConfig::RequiredAny => ToolChoiceMode::RequiredAny,
            ToolChoiceModeConfig::Specific { tool_id } => ToolChoiceMode::Specific {
                tool_name: ToolName::try_new(tool_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid tool choice tool id: {error}"))
                })?,
            },
        },
        disable_parallel_tool_use: choice.disable_parallel_tool_use,
    })
}

fn tool_choice_patch_from_api(
    patch: FieldPatch<ToolChoiceConfig>,
) -> Result<OptionalConfigPatch<ToolChoice>, AgentApiError> {
    match patch {
        FieldPatch::Set(choice) => Ok(OptionalConfigPatch::Set(tool_choice_from_api(choice)?)),
        FieldPatch::Clear => Ok(OptionalConfigPatch::Clear),
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
    })
}

pub(super) fn provider_params_with_reasoning(
    api_kind: &ProviderApiKind,
    base: Option<&ProviderParams>,
    effort: ReasoningEffort,
) -> Result<ProviderParams, AgentApiError> {
    match api_kind {
        ProviderApiKind::OpenAiResponses => {
            let mut params = llm_runtime::params::openai_responses_params(base)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            params.reasoning = match effort {
                ReasoningEffort::None => None,
                ReasoningEffort::Low => Some(openai_reasoning("low")),
                ReasoningEffort::Medium => Some(openai_reasoning("medium")),
                ReasoningEffort::High => Some(openai_reasoning("high")),
            };
            Ok(ProviderParams::new(
                ProviderApiKind::OpenAiResponses,
                serde_json::to_value(&params)
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?,
            ))
        }
        ProviderApiKind::AnthropicMessages => {
            let mut params = llm_runtime::params::anthropic_messages_params(base)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            // Current Anthropic models steer thinking through adaptive
            // thinking plus an output effort level, not token budgets.
            let effort_level = match effort {
                ReasoningEffort::None => None,
                ReasoningEffort::Low => Some("low"),
                ReasoningEffort::Medium => Some("medium"),
                ReasoningEffort::High => Some("high"),
            };
            params.thinking = effort_level.map(|_| anthropic_adaptive_thinking());
            params.output_config = effort_level.map(|level| serde_json::json!({ "effort": level }));
            Ok(ProviderParams::new(
                ProviderApiKind::AnthropicMessages,
                serde_json::to_value(&params)
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?,
            ))
        }
        ProviderApiKind::OpenAiCompletions => Err(AgentApiError::invalid_request(
            "reasoning effort is not supported for openai:completions",
        )),
    }
}

pub(super) fn openai_reasoning(effort: &str) -> llm_runtime::OpenAiReasoningConfig {
    llm_runtime::OpenAiReasoningConfig {
        effort: Some(effort.to_owned()),
        summary: Some("auto".to_owned()),
        extra: BTreeMap::new(),
    }
}

fn anthropic_adaptive_thinking() -> llm_runtime::AnthropicThinkingConfig {
    llm_runtime::AnthropicThinkingConfig {
        r#type: "adaptive".to_owned(),
        budget_tokens: None,
        display: None,
        extra: BTreeMap::new(),
    }
}
