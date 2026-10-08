use super::*;

impl GatewayAgentApi {
    pub(super) async fn session_config_for_start(
        &self,
        api_config: Option<api::SessionConfig>,
    ) -> Result<SessionConfig, AgentApiError> {
        let api_config = api_config.unwrap_or_default();
        let model = model_defaults::creation_model(api_config.model.clone(), async {
            self.store
                .read_model_defaults()
                .await
                .map_err(model_defaults::map_store_error)
        })
        .await?;
        let mut config = harness_session_config_from_api(api_config, model)?;
        self.resolve_context_capacity(&mut config).await;
        config
            .validate()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        self.admit_attachments(&config.features).await?;
        self.validate_workspace_attachment_targets(&config.features)
            .await?;
        self.validate_subagent_agents(&config.features).await?;
        Ok(config)
    }

    pub(super) async fn resolve_context_capacity(&self, config: &mut SessionConfig) {
        if config.context.input_limit_tokens.is_none() {
            config.context.reported_input_limit_tokens =
                self.model_discovery.input_limit(&config.model).await;
        }
    }

    /// Every allowlisted sub-agent profile must exist when the grant is
    /// admitted; the list is the authority, so a dangling id is a config
    /// error, not a spawn-time surprise.
    pub(super) async fn validate_subagent_agents(
        &self,
        features: &harness::FeaturesConfig,
    ) -> Result<(), AgentApiError> {
        let Some(subagents) = features.subagents.as_ref() else {
            return Ok(());
        };
        for agent in &subagents.agents {
            let profile_id =
                api::ProfileId::try_new(agent.profile_id.clone()).map_err(|error| {
                    AgentApiError::invalid_request(format!(
                        "invalid subagents agent profile id {:?}: {error}",
                        agent.profile_id
                    ))
                })?;
            self.read_profile(ProfileReadParams { profile_id })
                .await
                .map_err(|error| {
                    if is_not_found(&error) {
                        AgentApiError::invalid_request(format!(
                            "subagents agent profile does not exist: {}",
                            agent.profile_id
                        ))
                    } else {
                        error
                    }
                })?;
        }
        Ok(())
    }
}

/// Build only the per-run overrides supplied by `session/runs/start`.
/// Session defaults remain in `SessionConfig` and are resolved by the harness
/// from its live state; they must not be copied into the override document.
pub(super) fn run_config_for_start(
    session_config: &SessionConfig,
    api_config: Option<RunStartConfig>,
) -> Result<RunConfig, AgentApiError> {
    let mut run_config = RunConfig::default();
    apply_run_start_config(&mut run_config, session_config, api_config)?;
    Ok(run_config)
}

/// Translate the wire config document into the harness document. An absent
/// `model` uses the caller's resolved creation model or current session model;
/// everything else maps 1:1.
pub(super) fn harness_session_config_from_api(
    api_config: api::SessionConfig,
    resolved_model: ModelSelection,
) -> Result<SessionConfig, AgentApiError> {
    let model = match api_config.model {
        Some(model) => model_selection_from_api(model)?,
        None => resolved_model,
    };
    let generation = generation_from_api(api_config.generation, &model)?;
    Ok(SessionConfig {
        model,
        generation,
        limits: api_config
            .limits
            .map(|limits| harness::LimitsConfig {
                max_turns: limits.max_turns,
                max_tool_rounds: limits.max_tool_rounds,
            })
            .unwrap_or_default(),
        context: harness::ContextConfig {
            reported_input_limit_tokens: None,
            input_limit_tokens: api_config
                .context
                .as_ref()
                .and_then(|context| context.input_limit_tokens),
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
    model: &ModelSelection,
) -> Result<harness::GenerationConfig, AgentApiError> {
    let Some(generation) = generation else {
        return Ok(harness::GenerationConfig::default());
    };
    if let Some(effort) = generation.reasoning_effort.as_deref() {
        validate_reasoning_effort(&model.api_kind, effort)?;
    }
    let processing_tier = generation
        .processing_tier
        .map(|tier| processing_tier_from_api(model, tier))
        .transpose()?;
    Ok(harness::GenerationConfig {
        max_output_tokens: generation.max_output_tokens,
        reasoning_effort: generation.reasoning_effort,
        tool_choice: generation
            .tool_choice
            .map(tool_choice_from_api)
            .transpose()?,
        parallel_tool_use: generation.parallel_tool_use,
        processing_tier,
    })
}

fn processing_tier_from_api(
    model: &ModelSelection,
    tier: api::ModelProcessingTier,
) -> Result<harness::ModelProcessingTier, AgentApiError> {
    if model.provider_id != "openai"
        || !matches!(
            model.api_kind,
            ProviderApiKind::OpenAiResponses | ProviderApiKind::OpenAiCompletions
        )
    {
        return Err(AgentApiError::invalid_request(
            "generation.processingTier is supported only by the built-in openai provider",
        ));
    }
    Ok(match tier {
        api::ModelProcessingTier::Standard => harness::ModelProcessingTier::Standard,
        api::ModelProcessingTier::Fast => harness::ModelProcessingTier::Fast,
        api::ModelProcessingTier::Flex => harness::ModelProcessingTier::Flex,
    })
}

fn features_from_api(
    features: Option<api::FeaturesConfig>,
) -> Result<harness::FeaturesConfig, AgentApiError> {
    let Some(features) = features else {
        return Ok(harness::FeaturesConfig::default());
    };
    Ok(harness::FeaturesConfig {
        vfs: features
            .vfs
            .map(|vfs| {
                Ok::<_, AgentApiError>(harness::VfsFeature {
                    version: vfs.version,
                    working_directory: vfs.working_directory,
                    workspaces: vfs
                        .workspaces
                        .into_iter()
                        .map(workspace_attachment_from_api)
                        .collect::<Result<Vec<_>, _>>()?,
                    prompts: vfs.prompts.map(|prompts| harness::VfsPromptsConfig {
                        roots: prompts.roots,
                    }),
                    skills: vfs.skills.map(|skills| harness::VfsSkillsConfig {
                        roots: skills.roots,
                    }),
                })
            })
            .transpose()?,
        web: features.web.map(|web| harness::WebFeature {
            version: web.version,
            fetch: web.fetch.map(|_| harness::WebFetchFeature {}),
            search: web.search.map(|search| harness::WebSearchFeature {
                allowed_domains: search.allowed_domains,
                blocked_domains: search.blocked_domains,
            }),
        }),
        subagents: features
            .subagents
            .map(|subagents| harness::SubagentsFeature {
                version: subagents.version,
                agents: subagents
                    .agents
                    .into_iter()
                    .map(|agent| harness::SubagentAgentConfig {
                        profile_id: agent.profile_id.as_str().to_owned(),
                    })
                    .collect(),
                limits: harness::SubagentLimits {
                    max_depth: subagents.max_depth,
                    max_descendants: subagents.max_descendants,
                    max_concurrent: subagents.max_concurrent,
                    deadline_ms: subagents.deadline_ms,
                },
            }),
        code_mode: features.code_mode.map(|code| harness::CodeModeFeature {
            version: code.version,
            allowed_tools: code.allowed_tools,
            limits: harness::CodeModeLimits {
                timeout_ms: code.timeout_ms,
                max_memory_bytes: code.max_memory_bytes,
                max_stack_bytes: code.max_stack_bytes,
                max_source_bytes: code.max_source_bytes,
                max_catalog_bytes: code.max_catalog_bytes,
                max_request_bytes: code.max_request_bytes,
                max_result_bytes: code.max_result_bytes,
                max_output_bytes: code.max_output_bytes,
                max_tool_calls: code.max_tool_calls,
                max_outstanding_tool_calls: code.max_outstanding_tool_calls,
            },
        }),
        timers: features.timers.map(|timers| harness::TimersFeature {
            version: timers.version,
        }),
        environments: features
            .environments
            .map(|environments| {
                Ok::<_, AgentApiError>(harness::EnvironmentsFeature {
                    version: environments.version,
                    selection: environments.selection,
                    prompts: environments
                        .prompts
                        .map(|source| harness::EnvironmentPromptsConfig {
                            roots: source.roots,
                        }),
                    skills: environments
                        .skills
                        .map(|skills| harness::EnvironmentSkillsConfig {
                            roots: skills.roots,
                        }),
                    environments: environments
                        .environments
                        .into_iter()
                        .map(environment_attachment_from_api)
                        .collect::<Result<Vec<_>, _>>()?,
                })
            })
            .transpose()?,
        mcp: features.mcp.map(|mcp| harness::McpFeature {
            version: mcp.version,
            servers: mcp
                .servers
                .into_iter()
                .map(|attachment| harness::McpServerAttachment {
                    server_id: attachment.server_id,
                    tools: attachment.tools,
                })
                .collect(),
        }),
    })
}

fn workspace_attachment_from_api(
    attachment: api::WorkspaceAttachment,
) -> Result<harness::WorkspaceAttachment, AgentApiError> {
    let target = match (attachment.workspace_id, attachment.snapshot_ref) {
        (Some(workspace_id), None) => {
            harness::WorkspaceAttachmentTarget::Workspace { workspace_id }
        }
        (None, Some(snapshot_ref)) => harness::WorkspaceAttachmentTarget::Snapshot { snapshot_ref },
        _ => {
            return Err(AgentApiError::invalid_request(format!(
                "workspace attachment at {} must set exactly one of workspaceId and snapshotRef",
                attachment.path
            )));
        }
    };
    Ok(harness::WorkspaceAttachment {
        path: attachment.path,
        target,
        access: match attachment.access {
            api::WorkspaceAccess::Read => harness::WorkspaceAccess::Read,
            api::WorkspaceAccess::Edit => harness::WorkspaceAccess::Edit,
        },
    })
}

/// A session configuration names concrete machines only. `inherit` is a
/// profile-document notion resolved at sub-agent spawn, so it is rejected
/// here rather than silently dropped.
fn environment_attachment_from_api(
    attachment: api::EnvironmentAttachment,
) -> Result<harness::EnvironmentAttachment, AgentApiError> {
    let environment_id = match (attachment.environment_id, attachment.inherit) {
        (Some(environment_id), false) => environment_id,
        (None, true) => {
            return Err(AgentApiError::invalid_request(
                "environment attachment inherit is resolved when a sub-agent profile is spawned; a session configuration must name the environment",
            ));
        }
        _ => {
            return Err(AgentApiError::invalid_request(
                "environment attachment must set exactly one of environmentId and inherit",
            ));
        }
    };
    Ok(harness::EnvironmentAttachment {
        environment_id,
        default: attachment.default,
        access: environment_access_from_api(attachment.access),
        working_directory: attachment.working_directory,
    })
}

pub(super) fn environment_access_from_api(
    access: api::EnvironmentAccess,
) -> harness::EnvironmentAccess {
    match access {
        api::EnvironmentAccess::Read => harness::EnvironmentAccess::Read,
        api::EnvironmentAccess::Edit => harness::EnvironmentAccess::Edit,
        api::EnvironmentAccess::Exec => harness::EnvironmentAccess::Exec,
        api::EnvironmentAccess::Jobs => harness::EnvironmentAccess::Jobs,
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
    let RunStartConfig {
        model,
        generation,
        limits,
    } = api_config;
    let effective_model = if let Some(model) = model {
        let model = model_selection_from_api(model)?;
        run_config.model_override = Some(model.clone());
        model
    } else {
        session_config.model.clone()
    };
    if let Some(generation) = generation {
        if let Some(max_output_tokens) = generation.max_output_tokens {
            run_config.max_output_tokens = Some(max_output_tokens);
        }
        if let Some(effort) = generation.reasoning_effort {
            validate_reasoning_effort(&effective_model.api_kind, &effort)?;
            run_config.reasoning_effort = Some(effort);
        }
        if let Some(tool_choice) = generation.tool_choice {
            run_config.tool_choice = Some(tool_choice_from_api(tool_choice)?);
        }
        if let Some(parallel_tool_use) = generation.parallel_tool_use {
            run_config.parallel_tool_use = Some(parallel_tool_use);
        }
        if let Some(processing_tier) = generation.processing_tier {
            run_config.processing_tier =
                Some(processing_tier_from_api(&effective_model, processing_tier)?);
        }
    }
    if let Some(limits) = limits {
        apply_run_limits_config(run_config, limits);
    }
    run_config
        .validate_provider_compatibility(&session_config.model)
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
/// generation; the vocabulary is the runtime adapters' own so admission
/// never accepts a tier the adapter rejects (or the reverse).
pub(super) fn validate_reasoning_effort(
    api_kind: &ProviderApiKind,
    effort: &str,
) -> Result<(), AgentApiError> {
    let supported: &[&str] = match api_kind {
        ProviderApiKind::OpenAiResponses => llm_runtime::params::OPENAI_REASONING_EFFORT_TIERS,
        ProviderApiKind::AnthropicMessages => llm_runtime::params::ANTHROPIC_REASONING_EFFORT_TIERS,
        ProviderApiKind::OpenAiCompletions => {
            llm_runtime::params::OPENAI_COMPLETIONS_REASONING_EFFORT_TIERS
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_model_override_keeps_provider_identity_and_api_kind() {
        let model = ModelSelection {
            provider_id: "deepseek".into(),
            api_kind: ProviderApiKind::OpenAiCompletions,
            model: "deepseek-model".into(),
        };
        let session =
            harness_session_config_from_api(api::SessionConfig::default(), model).unwrap();
        for (provider, kind, allowed) in [
            ("deepseek", "openai:completions", true),
            ("glm", "openai:completions", false),
            ("deepseek", "openai:responses", false),
        ] {
            let result = apply_run_start_config(
                &mut RunConfig::default(),
                &session,
                Some(api::RunStartConfig {
                    model: Some(api::ModelConfig {
                        provider_id: provider.into(),
                        api_kind: kind.into(),
                        model: "another-model".into(),
                    }),
                    ..Default::default()
                }),
            );
            if allowed {
                result.unwrap();
            } else {
                assert!(
                    matches!(result, Err(error) if error.kind == api::AgentApiErrorKind::InvalidRequest)
                );
            }
        }
    }

    #[test]
    fn completions_accepts_current_openai_reasoning_vocabulary() {
        for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            validate_reasoning_effort(&ProviderApiKind::OpenAiCompletions, effort)
                .unwrap_or_else(|error| panic!("{effort} should be supported: {error}"));
        }
    }

    #[test]
    fn completions_rejects_unknown_reasoning_effort() {
        assert!(validate_reasoning_effort(&ProviderApiKind::OpenAiCompletions, "ultra").is_err());
    }

    #[test]
    fn anthropic_accepts_current_effort_vocabulary_including_xhigh() {
        for effort in ["none", "low", "medium", "high", "xhigh", "max"] {
            validate_reasoning_effort(&ProviderApiKind::AnthropicMessages, effort)
                .unwrap_or_else(|error| panic!("{effort} should be supported: {error}"));
        }
        assert!(validate_reasoning_effort(&ProviderApiKind::AnthropicMessages, "minimal").is_err());
    }

    #[test]
    fn admission_effort_vocabulary_matches_runtime_adapters() {
        for (api_kind, tiers) in [
            (
                ProviderApiKind::OpenAiResponses,
                llm_runtime::params::OPENAI_REASONING_EFFORT_TIERS,
            ),
            (
                ProviderApiKind::AnthropicMessages,
                llm_runtime::params::ANTHROPIC_REASONING_EFFORT_TIERS,
            ),
            (
                ProviderApiKind::OpenAiCompletions,
                llm_runtime::params::OPENAI_COMPLETIONS_REASONING_EFFORT_TIERS,
            ),
        ] {
            for effort in tiers {
                validate_reasoning_effort(&api_kind, effort)
                    .unwrap_or_else(|error| panic!("{api_kind:?} {effort}: {error}"));
            }
            assert!(validate_reasoning_effort(&api_kind, "ultra").is_err());
        }
    }
}
