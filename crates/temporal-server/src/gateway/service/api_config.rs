use super::*;

impl GatewayAgentApi {
    pub(super) async fn session_config_for_start(
        &self,
        api_config: Option<api::SessionConfig>,
    ) -> Result<SessionConfig, AgentApiError> {
        let config = engine_session_config_from_api(
            api_config.unwrap_or_default(),
            self.default_model.clone(),
        )?;
        config
            .validate()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        Ok(config)
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
        // Seed run budgets from the session's limits; generation defaults are
        // overlaid at planning time inside the engine.
        let mut run_config = RunConfig {
            max_turns: session_config.limits.max_turns,
            max_tool_rounds: session_config.limits.max_tool_rounds,
            ..RunConfig::default()
        };
        apply_run_start_config(&mut run_config, session_config, api_config)?;
        Ok(run_config)
    }
}

/// Translate the wire config document into the engine document. An absent
/// `model` falls back to the deployment default; everything else maps 1:1.
pub(super) fn engine_session_config_from_api(
    api_config: api::SessionConfig,
    default_model: ModelSelection,
) -> Result<SessionConfig, AgentApiError> {
    let model = match api_config.model {
        Some(model) => model_selection_from_api(model)?,
        None => default_model,
    };
    let generation = generation_from_api(api_config.generation, &model.api_kind)?;
    Ok(SessionConfig {
        model,
        generation,
        limits: api_config
            .limits
            .map(|limits| engine::LimitsConfig {
                max_turns: limits.max_turns,
                max_tool_rounds: limits.max_tool_rounds,
            })
            .unwrap_or_default(),
        context: engine::ContextConfig {
            compaction: api_config
                .context
                .and_then(|context| context.compaction)
                .map(compaction_policy_from_api),
        },
        features: features_from_api(api_config.features)?,
    })
}

fn generation_from_api(
    generation: Option<api::GenerationConfig>,
    api_kind: &ProviderApiKind,
) -> Result<engine::GenerationConfig, AgentApiError> {
    let Some(generation) = generation else {
        return Ok(engine::GenerationConfig::default());
    };
    if let Some(effort) = generation.reasoning_effort.as_deref() {
        validate_reasoning_effort(api_kind, effort)?;
    }
    Ok(engine::GenerationConfig {
        max_output_tokens: generation.max_output_tokens,
        reasoning_effort: generation.reasoning_effort,
        tool_choice: generation
            .tool_choice
            .map(tool_choice_from_api)
            .transpose()?,
        parallel_tool_use: generation.parallel_tool_use,
    })
}

fn features_from_api(
    features: Option<api::FeaturesConfig>,
) -> Result<engine::FeaturesConfig, AgentApiError> {
    let Some(features) = features else {
        return Ok(engine::FeaturesConfig::default());
    };
    Ok(engine::FeaturesConfig {
        vfs: features.vfs.map(|vfs| engine::VfsFeature {
            version: vfs.version,
            tools: vfs.tools.map(|tools| match tools {
                api::VfsToolSurface::ReadOnly => engine::VfsToolSurface::ReadOnly,
                api::VfsToolSurface::Edit => engine::VfsToolSurface::Edit,
            }),
            prompts: vfs.prompts.map(|prompts| engine::VfsPromptsConfig {
                roots: prompts.roots,
            }),
            skills: vfs.skills.map(|skills| engine::VfsSkillsConfig {
                roots: skills.roots,
            }),
        }),
        web: features.web.map(|web| engine::WebFeature {
            version: web.version,
            fetch: web.fetch.map(|_| engine::WebFetchFeature {}),
            search: web.search.map(|search| engine::WebSearchFeature {
                allowed_domains: search.allowed_domains,
                blocked_domains: search.blocked_domains,
            }),
        }),
        messaging: features
            .messaging
            .map(|messaging| engine::MessagingFeature {
                version: messaging.version,
            }),
        fleet: features.fleet.map(|fleet| engine::FleetFeature {
            version: fleet.version,
            profiles: fleet
                .profiles
                .map(fleet_profiles_config_from_api)
                .unwrap_or_default(),
            spawn: fleet
                .spawn
                .map(fleet_spawn_config_from_api)
                .unwrap_or_default(),
        }),
        timers: features.timers.map(|timers| engine::TimersFeature {
            version: timers.version,
        }),
        environments: features
            .environments
            .map(|environments| engine::EnvironmentsFeature {
                version: environments.version,
                providers: environments.providers,
            }),
        mcp: features.mcp.map(|mcp| engine::McpFeature {
            version: mcp.version,
            servers: mcp
                .servers
                .into_iter()
                .map(|link| engine::McpServerLink {
                    server_id: link.server_id,
                    allowed_tools: link.allowed_tools,
                    approval: link.approval.map(engine_mcp_approval),
                    defer_loading: link.defer_loading,
                    auth_grant_id: link.auth_grant_id,
                })
                .collect(),
        }),
    })
}

fn engine_mcp_approval(policy: api::RemoteMcpApprovalPolicy) -> engine::RemoteMcpApprovalPolicy {
    match policy {
        api::RemoteMcpApprovalPolicy::ProviderDefault => {
            engine::RemoteMcpApprovalPolicy::ProviderDefault
        }
        api::RemoteMcpApprovalPolicy::Always => engine::RemoteMcpApprovalPolicy::Always,
        api::RemoteMcpApprovalPolicy::Never => engine::RemoteMcpApprovalPolicy::Never,
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
            validate_reasoning_effort(&effective_api_kind, &effort)?;
            run_config.reasoning_effort = Some(effort);
        }
        if let Some(tool_choice) = generation.tool_choice {
            run_config.tool_choice = Some(tool_choice_from_api(tool_choice)?);
        }
        if let Some(parallel_tool_use) = generation.parallel_tool_use {
            run_config.parallel_tool_use = Some(parallel_tool_use);
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

/// Known reasoning effort tiers per provider api kind. Enforced at the
/// admission boundary so typos fail the put/run request, not the first
/// generation; the runtime adapters validate again when materializing.
pub(super) fn validate_reasoning_effort(
    api_kind: &ProviderApiKind,
    effort: &str,
) -> Result<(), AgentApiError> {
    let supported: &[&str] = match api_kind {
        ProviderApiKind::OpenAiResponses => &["none", "low", "medium", "high", "xhigh"],
        ProviderApiKind::AnthropicMessages => &["none", "low", "medium", "high", "max"],
        ProviderApiKind::OpenAiCompletions => {
            return Err(AgentApiError::invalid_request(
                "reasoning effort is not supported for openai:completions",
            ));
        }
    };
    if supported.contains(&effort) {
        Ok(())
    } else {
        Err(AgentApiError::invalid_request(format!(
            "unsupported reasoning effort {effort:?} for {api_kind:?}; supported: {}",
            supported.join(", ")
        )))
    }
}

fn fleet_profiles_config_from_api(
    profiles: api::FleetProfilesConfig,
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

fn fleet_spawn_config_from_api(spawn: api::FleetSpawnConfig) -> engine::FleetSpawnConfig {
    engine::FleetSpawnConfig {
        bases: spawn.bases.map(|bases| {
            bases
                .into_iter()
                .map(|base| match base {
                    api::FleetSpawnBase::Self_ => engine::FleetSpawnBase::Self_,
                    api::FleetSpawnBase::Session => engine::FleetSpawnBase::Session,
                    api::FleetSpawnBase::Profile => engine::FleetSpawnBase::Profile,
                })
                .collect()
        }),
    }
}

fn tool_choice_from_api(choice: api::ToolChoice) -> Result<ToolChoice, AgentApiError> {
    Ok(match choice {
        api::ToolChoice::Auto => ToolChoice::Auto,
        api::ToolChoice::None => ToolChoice::None,
        api::ToolChoice::RequiredAny => ToolChoice::RequiredAny,
        api::ToolChoice::Specific { tool_id } => ToolChoice::Specific {
            tool_name: ToolName::try_new(tool_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid tool choice tool id: {error}"))
            })?,
        },
    })
}

pub(super) fn compaction_policy_from_api(policy: api::CompactionPolicy) -> CompactionPolicy {
    match policy {
        api::CompactionPolicy::Disabled => CompactionPolicy::Disabled,
        api::CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        } => CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        },
        api::CompactionPolicy::ProviderStandalone {
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
