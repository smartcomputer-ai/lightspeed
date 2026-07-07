use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ActiveRun, CompactionPolicy, ContextSnapshot, CoreAgentState, DomainError, PlannedRequestState,
    PlanningError, RunConfig, RunId, SessionConfig, SessionId, ToolChoice, ToolChoiceMode,
    ToolKind, ToolSpec, TurnId,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderApiKind {
    OpenAiResponses,
    AnthropicMessages,
    OpenAiCompletions,
}

/// Deterministic model route: which provider API to speak, which configured
/// provider to use, and which model to request. Transport configuration
/// (endpoints, credentials, headers) is runtime deployment config keyed by
/// `provider_id` and never enters the session log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub api_kind: ProviderApiKind,
    pub provider_id: String,
    pub model: String,
}

/// Opaque provider request parameters.
///
/// The reducer and planners never read into `body`; it is carried through the
/// session log verbatim and parsed only by the runtime adapter that
/// materializes the wire request. Validation against the adapter's schema
/// happens at the admission boundary, before the params enter the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderParams {
    pub api_kind: ProviderApiKind,
    pub version: u32,
    pub body: Value,
}

impl ProviderParams {
    pub fn new(api_kind: ProviderApiKind, body: Value) -> Self {
        Self {
            api_kind,
            version: 1,
            body,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCompatibility {
    pub api_kind: ProviderApiKind,
    pub model: String,
    pub native_context_family: String,
}

/// Transient generation intent planned by the core.
///
/// This is rebuilt from reduced state when a generation action is emitted; it is
/// not stored in the durable turn log or reduced turn state. Provider-specific
/// request settings travel opaquely in `params`; runtime adapters materialize
/// the provider-native wire request from this intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: ModelSelection,
    pub request_fingerprint: String,
    pub context: ContextSnapshot,
    pub tools: Vec<ToolSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_limit: Option<u32>,
    /// Provider continuity token (e.g. OpenAI Responses `previous_response_id`)
    /// threaded from prior generation facts. Currently always `None`; adapters
    /// must tolerate absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_response_id: Option<String>,
    /// Session compaction policy at planning time, so adapters can lower
    /// provider-managed compaction (e.g. OpenAI `context_management`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<ProviderParams>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionRequest {
    pub session_id: SessionId,
    pub request: ContextCompactionTask,
}

/// Deterministic compaction intent planned by the core.
///
/// Like [`LlmRequest`], provider-specific settings stay opaque in `params`;
/// adapters that do not support standalone compaction must fail the request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionTask {
    pub model: ModelSelection,
    pub request_fingerprint: String,
    pub context: ContextSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<ProviderParams>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionResult {
    pub session_id: SessionId,
    pub context_revision: u64,
    pub status: crate::ContextCompactionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_ref: Option<crate::BlobRef>,
    pub context_entries: Vec<crate::ContextEntryInput>,
}

pub(crate) fn build_llm_request(
    state: &CoreAgentState,
    active_run: &ActiveRun,
    turn_id: TurnId,
) -> Result<LlmRequest, PlanningError> {
    let Some(config) = state.lifecycle.config.as_ref() else {
        return Err(
            DomainError::InvariantViolation("active session is missing config".to_owned()).into(),
        );
    };
    let model = active_run
        .run_config
        .model_override
        .clone()
        .unwrap_or_else(|| config.model.clone());
    let request_config = session_config_for_run(config, &active_run.run_config);
    let context =
        crate::core::components::context::planned_context_snapshot(state, model.api_kind.clone())?;
    let tools = active_tools(state, &model.api_kind)?;
    let tool_choice = request_config.turn.tool_choice.clone();
    validate_tool_choice(&tools, tool_choice.as_ref())?;
    let params = request_config.turn.provider_params.clone();
    if let Some(params) = params.as_ref()
        && params.api_kind != model.api_kind
    {
        return Err(DomainError::ProviderCompatibility(format!(
            "provider params api kind {:?} do not match model api kind {:?}",
            params.api_kind, model.api_kind
        ))
        .into());
    }
    let output_limit = request_config.turn.max_output_tokens;
    let compaction = request_config.context.compaction.clone();
    let request_fingerprint = request_fingerprint(
        &model,
        &context,
        &tools,
        tool_choice.as_ref(),
        output_limit,
        params.as_ref(),
        active_run.run_id,
        turn_id,
    )?;
    Ok(LlmRequest {
        model,
        request_fingerprint,
        context,
        tools,
        tool_choice,
        output_limit,
        provider_response_id: None,
        compaction,
        params,
    })
}

pub(crate) fn build_planned_llm_request(
    state: &CoreAgentState,
    active_run: &ActiveRun,
    turn_id: TurnId,
    planned: &PlannedRequestState,
) -> Result<LlmRequest, DomainError> {
    if planned.config_revision != state.lifecycle.config_revision {
        return Err(DomainError::InvariantViolation(format!(
            "planned request config revision {} does not match active revision {}",
            planned.config_revision, state.lifecycle.config_revision
        )));
    }
    if planned.context_revision != state.context.revision {
        return Err(DomainError::InvariantViolation(format!(
            "planned request context revision {} does not match active revision {}",
            planned.context_revision, state.context.revision
        )));
    }
    if planned.toolset_revision != state.tooling.revision {
        return Err(DomainError::InvariantViolation(format!(
            "planned request toolset revision {} does not match active revision {}",
            planned.toolset_revision, state.tooling.revision
        )));
    }
    let request = build_llm_request(state, active_run, turn_id)
        .map_err(|error| DomainError::InvariantViolation(error.to_string()))?;
    if request.request_fingerprint != planned.request_fingerprint {
        return Err(DomainError::InvariantViolation(format!(
            "rebuilt request fingerprint {} does not match planned fingerprint {}",
            request.request_fingerprint, planned.request_fingerprint
        )));
    }
    Ok(request)
}

pub(crate) fn build_context_compaction_task(
    state: &CoreAgentState,
) -> Result<ContextCompactionTask, PlanningError> {
    let Some(config) = state.lifecycle.config.as_ref() else {
        return Err(
            DomainError::InvariantViolation("active session is missing config".to_owned()).into(),
        );
    };
    if !state.context.pending_compaction {
        return Err(DomainError::InvariantViolation(
            "context compaction request is missing pending state".to_owned(),
        )
        .into());
    }
    let CompactionPolicy::ProviderStandalone { target_tokens, .. } =
        config.context.compaction.as_ref().ok_or_else(|| {
            DomainError::ProviderCompatibility(
                "pending context compaction requires provider-standalone policy".to_owned(),
            )
        })?
    else {
        return Err(DomainError::ProviderCompatibility(
            "pending context compaction requires provider-standalone policy".to_owned(),
        )
        .into());
    };
    let context = crate::core::components::context::compactable_context_snapshot(
        state,
        config.model.api_kind.clone(),
    )?;
    let params = config.turn.provider_params.clone();
    let request_fingerprint =
        compaction_request_fingerprint(&config.model, &context, *target_tokens, params.as_ref())?;
    Ok(ContextCompactionTask {
        model: config.model.clone(),
        request_fingerprint,
        context,
        target_tokens: *target_tokens,
        params,
    })
}

fn session_config_for_run(config: &SessionConfig, run_config: &RunConfig) -> SessionConfig {
    let mut config = config.clone();
    if let Some(max_output_tokens) = run_config.max_output_tokens {
        config.turn.max_output_tokens = Some(max_output_tokens);
    }
    if let Some(params) = run_config.provider_params.clone() {
        config.turn.provider_params = Some(params);
    }
    if let Some(tool_choice) = run_config.tool_choice.clone() {
        config.turn.tool_choice = Some(tool_choice);
    }
    config
}

fn active_tools(
    state: &CoreAgentState,
    api_kind: &ProviderApiKind,
) -> Result<Vec<ToolSpec>, PlanningError> {
    let mut tools = Vec::with_capacity(state.tooling.tools.len());
    for tool in state.tooling.tools.values() {
        match &tool.kind {
            ToolKind::ProviderNative(native) => {
                if native.api_kind != *api_kind {
                    return Err(DomainError::ProviderCompatibility(format!(
                        "provider-native tool {} api kind {:?} does not match request api kind {:?}",
                        tool.name, native.api_kind, api_kind
                    ))
                    .into());
                }
            }
            ToolKind::RemoteMcp(_) => {
                if !remote_mcp_supported_by_provider(api_kind) {
                    return Err(DomainError::ProviderCompatibility(format!(
                        "remote MCP tool {} is not supported by request api kind {:?}",
                        tool.name, api_kind
                    ))
                    .into());
                }
            }
            ToolKind::Function(_) => {}
        }
        tools.push(tool.clone());
    }

    Ok(tools)
}

fn validate_tool_choice(
    tools: &[ToolSpec],
    tool_choice: Option<&ToolChoice>,
) -> Result<(), PlanningError> {
    let Some(ToolChoice {
        mode: ToolChoiceMode::Specific { tool_name },
        ..
    }) = tool_choice
    else {
        return Ok(());
    };
    if tools.iter().any(|tool| &tool.name == tool_name) {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "tool_choice references missing active tool {}",
            tool_name
        ))
        .into())
    }
}

fn remote_mcp_supported_by_provider(api_kind: &ProviderApiKind) -> bool {
    matches!(
        api_kind,
        ProviderApiKind::OpenAiResponses | ProviderApiKind::AnthropicMessages
    )
}

#[expect(clippy::too_many_arguments)]
fn request_fingerprint(
    model: &ModelSelection,
    context: &ContextSnapshot,
    tools: &[ToolSpec],
    tool_choice: Option<&ToolChoice>,
    output_limit: Option<u32>,
    params: Option<&ProviderParams>,
    run_id: RunId,
    turn_id: TurnId,
) -> Result<String, PlanningError> {
    let encoded = serde_json::to_vec(&(
        model,
        context,
        tools,
        tool_choice,
        output_limit,
        params,
        run_id,
        turn_id,
    ))
    .map_err(|error| PlanningError::Rejected(format!("failed to fingerprint request: {error}")))?;
    let digest = Sha256::digest(encoded);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

fn compaction_request_fingerprint(
    model: &ModelSelection,
    context: &ContextSnapshot,
    target_tokens: Option<u32>,
    params: Option<&ProviderParams>,
) -> Result<String, PlanningError> {
    let encoded =
        serde_json::to_vec(&(model, context, target_tokens, params)).map_err(|error| {
            PlanningError::Rejected(format!("failed to fingerprint compaction request: {error}"))
        })?;
    let digest = Sha256::digest(encoded);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            run: RunConfig::default(),
            turn: crate::TurnConfig {
                max_output_tokens: None,
                tool_choice: None,
                provider_params: None,
            },
            context: crate::ContextConfig { compaction: None },
            tools: Default::default(),
            fleet: Default::default(),
        }
    }

    #[test]
    fn session_config_for_run_applies_generation_overrides() {
        let config = test_config();
        let params = ProviderParams::new(
            ProviderApiKind::OpenAiResponses,
            serde_json::json!({ "reasoning": { "effort": "high" } }),
        );
        let run_config = RunConfig {
            max_output_tokens: Some(2048),
            provider_params: Some(params.clone()),
            ..RunConfig::default()
        };

        let resolved = session_config_for_run(&config, &run_config);

        assert_eq!(resolved.turn.max_output_tokens, Some(2048));
        assert_eq!(resolved.turn.provider_params, Some(params));
    }

    #[test]
    fn provider_native_tool_rejects_mismatched_request_api_kind() {
        let mut state = CoreAgentState::new();
        let tool_name = crate::ToolName::new("web_search");
        state.tooling.tools.insert(
            tool_name.clone(),
            ToolSpec {
                name: tool_name.clone(),
                kind: ToolKind::ProviderNative(crate::ProviderNativeToolSpec {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    native_tool_ref: crate::BlobRef::from_bytes(
                        br#"{"type":"web_search","external_web_access":false}"#,
                    ),
                    execution: crate::ProviderNativeToolExecution::ProviderHosted,
                }),
                parallelism: crate::ToolParallelism::ParallelSafe,
                target_requirement: crate::ToolTargetRequirement::None,
            },
        );

        let error = active_tools(&state, &ProviderApiKind::AnthropicMessages)
            .expect_err("provider-native tool must reject mismatched api kind");

        let PlanningError::Domain(DomainError::ProviderCompatibility(message)) = error else {
            panic!("expected provider compatibility error, got {error:?}");
        };
        assert!(message.contains("provider-native tool web_search"));
        assert!(message.contains("OpenAiResponses"));
        assert!(message.contains("AnthropicMessages"));
    }

    fn remote_mcp_tool(auth_ref_id: &str) -> ToolSpec {
        ToolSpec {
            name: crate::ToolName::new("mcp_echo"),
            kind: ToolKind::RemoteMcp(crate::RemoteMcpToolSpec {
                server_label: "echo".to_owned(),
                server_url: "https://echo.example.com/mcp".to_owned(),
                description_ref: None,
                allowed_tools: Some(vec!["hello".to_owned()]),
                approval: crate::RemoteMcpApprovalPolicy::Never,
                defer_loading: Some(true),
                auth_ref: Some(crate::SecretRef {
                    namespace: "mcp_grant".to_owned(),
                    id: auth_ref_id.to_owned(),
                }),
            }),
            parallelism: crate::ToolParallelism::ParallelSafe,
            target_requirement: crate::ToolTargetRequirement::None,
        }
    }

    fn state_with_remote_mcp_tool() -> CoreAgentState {
        let mut state = CoreAgentState::new();
        let tool = remote_mcp_tool("mcpgrant_123");
        let tool_name = tool.name.clone();
        state.tooling.tools.insert(tool_name, tool);
        state
    }

    #[test]
    fn remote_mcp_tool_selection_accepts_supported_provider_api_kinds() {
        let state = state_with_remote_mcp_tool();

        for api_kind in [
            ProviderApiKind::OpenAiResponses,
            ProviderApiKind::AnthropicMessages,
        ] {
            let tools = active_tools(&state, &api_kind)
                .expect("remote MCP should be selectable for supported providers");

            assert_eq!(tools.len(), 1);
            assert!(matches!(tools[0].kind, ToolKind::RemoteMcp(_)));
        }
    }

    #[test]
    fn remote_mcp_tool_selection_rejects_openai_completions() {
        let state = state_with_remote_mcp_tool();

        let error = active_tools(&state, &ProviderApiKind::OpenAiCompletions)
            .expect_err("remote MCP is not supported by OpenAI Completions");

        let PlanningError::Domain(DomainError::ProviderCompatibility(message)) = error else {
            panic!("expected provider compatibility error, got {error:?}");
        };
        assert!(message.contains("remote MCP tool mcp_echo"));
        assert!(message.contains("OpenAiCompletions"));
    }

    #[test]
    fn remote_mcp_sanitized_auth_ref_participates_in_request_fingerprint() {
        let config = test_config();
        let context = ContextSnapshot {
            api_kind: ProviderApiKind::OpenAiResponses,
            context_revision: 7,
            entries: Vec::new(),
            token_estimate: None,
        };

        let first_tools = vec![remote_mcp_tool("mcpgrant_123")];
        let second_tools = vec![remote_mcp_tool("mcpgrant_456")];

        let encoded = serde_json::to_string(&first_tools).expect("serialize tools");
        assert!(encoded.contains("mcp_grant"));
        assert!(encoded.contains("mcpgrant_123"));
        assert!(!encoded.contains("runtime-token"));

        let first_fingerprint = request_fingerprint(
            &config.model,
            &context,
            &first_tools,
            None,
            None,
            None,
            RunId::new(1),
            TurnId::new(1),
        )
        .expect("fingerprint first request");
        let second_fingerprint = request_fingerprint(
            &config.model,
            &context,
            &second_tools,
            None,
            None,
            None,
            RunId::new(1),
            TurnId::new(1),
        )
        .expect("fingerprint second request");

        assert_ne!(first_fingerprint, second_fingerprint);
    }
}
