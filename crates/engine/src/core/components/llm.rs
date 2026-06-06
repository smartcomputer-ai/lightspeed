use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    ActiveRun, CompactionPolicy, ContextSnapshot, CoreAgentState, DomainError, PlanningError,
    RunConfig, RunId, SessionConfig, SessionId, ToolChoice, ToolChoiceMode, ToolKind, ToolName,
    ToolSpec, TurnId,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderApiKind {
    OpenAiResponses,
    AnthropicMessages,
    OpenAiCompletions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub api_kind: ProviderApiKind,
    pub provider_id: String,
    pub model: String,
    pub options: ModelProviderOptions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderOptions {
    None,
    OpenAiResponses(OpenAiModelOptions),
    AnthropicMessages(AnthropicModelOptions),
    OpenAiCompletions(OpenAiModelOptions),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiModelOptions {
    pub organization: Option<String>,
    pub project: Option<String>,
    pub base_url: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicModelOptions {
    pub base_url: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRequestDefaults {
    None,
    OpenAiResponses(OpenAiResponsesRequestDefaults),
    AnthropicMessages(AnthropicMessagesRequestDefaults),
    OpenAiCompletions(OpenAiCompletionsRequestDefaults),
}

pub const OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE: &str =
    "reasoning.encrypted_content";
pub const OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE: &str = "web_search_call.action.sources";

fn default_openai_responses_include() -> Vec<String> {
    vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_owned()]
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiResponsesRequestDefaults {
    pub reasoning: Option<OpenAiReasoningConfig>,
    pub text: Option<Value>,
    #[serde(default = "default_openai_responses_include")]
    pub include: Vec<String>,
    pub temperature: Option<Value>,
    pub top_p: Option<Value>,
    pub metadata: BTreeMap<String, String>,
    pub parallel_tool_calls: Option<bool>,
    pub store: Option<bool>,
    pub stream: Option<bool>,
    pub truncation: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

impl Default for OpenAiResponsesRequestDefaults {
    fn default() -> Self {
        Self {
            reasoning: None,
            text: None,
            include: default_openai_responses_include(),
            temperature: None,
            top_p: None,
            metadata: BTreeMap::new(),
            parallel_tool_calls: None,
            store: None,
            stream: None,
            truncation: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicMessagesRequestDefaults {
    pub thinking: Option<AnthropicThinkingConfig>,
    pub metadata: Option<Value>,
    pub stop_sequences: Vec<String>,
    pub stream: Option<bool>,
    pub temperature: Option<Value>,
    pub top_k: Option<u32>,
    pub top_p: Option<Value>,
    pub service_tier: Option<String>,
    pub container: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCompletionsRequestDefaults {
    pub response_format: Option<Value>,
    pub temperature: Option<Value>,
    pub top_p: Option<Value>,
    pub stop: Option<Value>,
    pub parallel_tool_calls: Option<bool>,
    pub store: Option<bool>,
    pub stream: Option<bool>,
    pub metadata: BTreeMap<String, String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCompatibility {
    pub api_kind: ProviderApiKind,
    pub model: String,
    pub native_context_family: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: ModelSelection,
    pub request_fingerprint: String,
    pub kind: LlmRequestKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionRequest {
    pub session_id: SessionId,
    pub request: ContextCompactionTask,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionTask {
    pub model: ModelSelection,
    pub request_fingerprint: String,
    pub kind: ContextCompactionRequestKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionRequestKind {
    OpenAiResponses(OpenAiResponsesCompactionRequest),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiResponsesCompactionRequest {
    pub input_context: ContextSnapshot,
    pub target_tokens: Option<u32>,
    pub store: Option<bool>,
    pub extra: BTreeMap<String, Value>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmRequestKind {
    OpenAiResponses(OpenAiResponsesRequest),
    AnthropicMessages(AnthropicMessagesRequest),
    OpenAiCompletions(OpenAiCompletionsRequest),
}

pub(crate) fn validate_request_matches_active_context(
    state: &CoreAgentState,
    request: &LlmRequest,
) -> Result<(), DomainError> {
    let request_context = llm_request_context(request)?;
    crate::core::components::context::validate_snapshot_matches_active_context(
        state,
        request_context,
    )
}

pub(crate) fn llm_request_context(request: &LlmRequest) -> Result<&ContextSnapshot, DomainError> {
    let (expected_api_kind, context) = match &request.kind {
        LlmRequestKind::OpenAiResponses(request) => {
            (ProviderApiKind::OpenAiResponses, &request.input_context)
        }
        LlmRequestKind::AnthropicMessages(request) => (
            ProviderApiKind::AnthropicMessages,
            &request.messages_context,
        ),
        LlmRequestKind::OpenAiCompletions(request) => (
            ProviderApiKind::OpenAiCompletions,
            &request.messages_context,
        ),
    };
    if request.model.api_kind != expected_api_kind {
        return Err(DomainError::ProviderCompatibility(format!(
            "request kind {:?} does not match model api kind {:?}",
            expected_api_kind, request.model.api_kind
        )));
    }
    if context.api_kind != request.model.api_kind {
        return Err(DomainError::ProviderCompatibility(format!(
            "request context api kind {:?} does not match model api kind {:?}",
            context.api_kind, request.model.api_kind
        )));
    }
    Ok(context)
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
    let (tools, tool_choice) = selected_tools_and_choice(state, &model.api_kind)?;
    let kind = match (
        &model.api_kind,
        &request_config.turn.provider_request_defaults,
    ) {
        (ProviderApiKind::OpenAiResponses, ProviderRequestDefaults::None) => {
            LlmRequestKind::OpenAiResponses(openai_responses_request(
                &request_config,
                context.clone(),
                tools,
                tool_choice.as_ref(),
                &OpenAiResponsesRequestDefaults::default(),
            ))
        }
        (ProviderApiKind::OpenAiResponses, ProviderRequestDefaults::OpenAiResponses(defaults)) => {
            LlmRequestKind::OpenAiResponses(openai_responses_request(
                &request_config,
                context.clone(),
                tools,
                tool_choice.as_ref(),
                &defaults,
            ))
        }
        (ProviderApiKind::AnthropicMessages, ProviderRequestDefaults::None) => {
            LlmRequestKind::AnthropicMessages(anthropic_messages_request(
                &request_config,
                context.clone(),
                tools,
                tool_choice.as_ref(),
                &AnthropicMessagesRequestDefaults::default(),
            )?)
        }
        (
            ProviderApiKind::AnthropicMessages,
            ProviderRequestDefaults::AnthropicMessages(defaults),
        ) => LlmRequestKind::AnthropicMessages(anthropic_messages_request(
            &request_config,
            context.clone(),
            tools,
            tool_choice.as_ref(),
            &defaults,
        )?),
        (ProviderApiKind::OpenAiCompletions, ProviderRequestDefaults::None) => {
            LlmRequestKind::OpenAiCompletions(openai_completions_request(
                &request_config,
                context.clone(),
                tools,
                tool_choice.as_ref(),
                &OpenAiCompletionsRequestDefaults::default(),
            ))
        }
        (
            ProviderApiKind::OpenAiCompletions,
            ProviderRequestDefaults::OpenAiCompletions(defaults),
        ) => LlmRequestKind::OpenAiCompletions(openai_completions_request(
            &request_config,
            context,
            tools,
            tool_choice.as_ref(),
            &defaults,
        )),
        (_, defaults) => {
            return Err(DomainError::ProviderCompatibility(format!(
                "request defaults {:?} do not match model api kind {:?}",
                defaults, model.api_kind
            ))
            .into());
        }
    };
    let request_fingerprint = request_fingerprint(&model, &kind, active_run.run_id, turn_id)?;

    Ok(LlmRequest {
        model,
        request_fingerprint,
        kind,
    })
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
    let kind = match (
        &config.model.api_kind,
        &config.turn.provider_request_defaults,
    ) {
        (ProviderApiKind::OpenAiResponses, ProviderRequestDefaults::None) => {
            ContextCompactionRequestKind::OpenAiResponses(OpenAiResponsesCompactionRequest {
                input_context: context,
                target_tokens: *target_tokens,
                store: None,
                extra: BTreeMap::new(),
            })
        }
        (ProviderApiKind::OpenAiResponses, ProviderRequestDefaults::OpenAiResponses(_)) => {
            ContextCompactionRequestKind::OpenAiResponses(OpenAiResponsesCompactionRequest {
                input_context: context,
                target_tokens: *target_tokens,
                store: None,
                extra: BTreeMap::new(),
            })
        }
        (api_kind, _) => {
            return Err(DomainError::ProviderCompatibility(format!(
                "provider-standalone compaction requires OpenAI Responses api kind, got {:?}",
                api_kind
            ))
            .into());
        }
    };
    let request_fingerprint = compaction_request_fingerprint(&config.model, &kind)?;
    Ok(ContextCompactionTask {
        model: config.model.clone(),
        request_fingerprint,
        kind,
    })
}

fn session_config_for_run(config: &SessionConfig, run_config: &RunConfig) -> SessionConfig {
    let mut config = config.clone();
    if let Some(max_output_tokens) = run_config.max_output_tokens {
        config.turn.max_output_tokens = Some(max_output_tokens);
    }
    if let Some(defaults) = run_config.provider_request_defaults.clone() {
        config.turn.provider_request_defaults = defaults;
    }
    config
}

fn selected_tools_and_choice(
    state: &CoreAgentState,
    api_kind: &ProviderApiKind,
) -> Result<(Vec<ToolSpec>, Option<ToolChoice>), PlanningError> {
    let Some(profile_id) = state.tooling.selected_profile_id.as_ref() else {
        return Ok((Vec::new(), None));
    };
    let Some(profile) = state.tooling.registry.profiles.get(profile_id) else {
        return Err(DomainError::InvariantViolation(format!(
            "selected tool profile {} does not exist",
            profile_id
        ))
        .into());
    };

    let mut tools = Vec::with_capacity(profile.visible_tools.len());
    for tool_name in &profile.visible_tools {
        let Some(tool) = state.tooling.registry.tools.get(tool_name) else {
            return Err(DomainError::InvariantViolation(format!(
                "tool profile {} references missing tool {}",
                profile_id, tool_name
            ))
            .into());
        };
        if let ToolKind::ProviderNative(native) = &tool.kind {
            if native.api_kind != *api_kind {
                return Err(DomainError::ProviderCompatibility(format!(
                    "provider-native tool {} api kind {:?} does not match request api kind {:?}",
                    tool.name, native.api_kind, api_kind
                ))
                .into());
            }
        }
        tools.push(tool.clone());
    }

    Ok((tools, profile.tool_choice.clone()))
}

fn openai_responses_request(
    config: &SessionConfig,
    input_context: ContextSnapshot,
    tools: Vec<ToolSpec>,
    tool_choice: Option<&ToolChoice>,
    defaults: &OpenAiResponsesRequestDefaults,
) -> OpenAiResponsesRequest {
    OpenAiResponsesRequest {
        input_context,
        previous_response_id: None,
        tools,
        tool_choice: tool_choice.map(openai_responses_tool_choice),
        reasoning: defaults.reasoning.clone(),
        text: defaults.text.clone(),
        include: defaults.include.clone(),
        max_output_tokens: config.turn.max_output_tokens,
        max_tool_calls: None,
        temperature: defaults.temperature.clone(),
        top_p: defaults.top_p.clone(),
        metadata: defaults.metadata.clone(),
        parallel_tool_calls: defaults.parallel_tool_calls,
        store: defaults.store,
        stream: defaults.stream,
        truncation: defaults.truncation.clone(),
        context_management: openai_responses_context_management(config),
        extra: defaults.extra.clone(),
    }
}

fn openai_responses_context_management(config: &SessionConfig) -> Option<Value> {
    match &config.context.compaction {
        Some(CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        }) => {
            let mut compaction = json!({ "type": "compaction" });
            if let Some(compact_threshold_tokens) = compact_threshold_tokens {
                compaction["compact_threshold"] = json!(compact_threshold_tokens);
            }
            Some(json!([compaction]))
        }
        None | Some(CompactionPolicy::Disabled | CompactionPolicy::ProviderStandalone { .. }) => {
            None
        }
    }
}

fn anthropic_messages_request(
    config: &SessionConfig,
    messages_context: ContextSnapshot,
    tools: Vec<ToolSpec>,
    tool_choice: Option<&ToolChoice>,
    defaults: &AnthropicMessagesRequestDefaults,
) -> Result<AnthropicMessagesRequest, PlanningError> {
    let Some(max_tokens) = config.turn.max_output_tokens else {
        return Err(DomainError::ProviderCompatibility(
            "anthropic messages requests require TurnConfig::max_output_tokens".to_owned(),
        )
        .into());
    };
    Ok(AnthropicMessagesRequest {
        messages_context,
        tools,
        tool_choice: tool_choice.map(anthropic_tool_choice),
        thinking: defaults.thinking.clone(),
        max_tokens,
        metadata: defaults.metadata.clone(),
        stop_sequences: defaults.stop_sequences.clone(),
        stream: defaults.stream,
        temperature: defaults.temperature.clone(),
        top_k: defaults.top_k,
        top_p: defaults.top_p.clone(),
        service_tier: defaults.service_tier.clone(),
        container: defaults.container.clone(),
        mcp_servers: None,
        context_management: None,
        extra: defaults.extra.clone(),
    })
}

fn openai_completions_request(
    config: &SessionConfig,
    messages_context: ContextSnapshot,
    tools: Vec<ToolSpec>,
    tool_choice: Option<&ToolChoice>,
    defaults: &OpenAiCompletionsRequestDefaults,
) -> OpenAiCompletionsRequest {
    OpenAiCompletionsRequest {
        messages_context,
        tools,
        tool_choice: tool_choice.map(openai_completions_tool_choice),
        response_format: defaults.response_format.clone(),
        temperature: defaults.temperature.clone(),
        top_p: defaults.top_p.clone(),
        max_tokens: config.turn.max_output_tokens,
        max_completion_tokens: config.turn.max_output_tokens,
        stop: defaults.stop.clone(),
        parallel_tool_calls: defaults.parallel_tool_calls,
        store: defaults.store,
        stream: defaults.stream,
        metadata: defaults.metadata.clone(),
        extra: defaults.extra.clone(),
    }
}

fn openai_responses_tool_choice(choice: &ToolChoice) -> OpenAiResponsesToolChoice {
    match &choice.mode {
        ToolChoiceMode::Auto => OpenAiResponsesToolChoice::Auto,
        ToolChoiceMode::None => OpenAiResponsesToolChoice::None,
        ToolChoiceMode::RequiredAny => OpenAiResponsesToolChoice::Required,
        ToolChoiceMode::Specific { tool_name } => OpenAiResponsesToolChoice::Function {
            name: tool_name.clone(),
        },
    }
}

fn anthropic_tool_choice(choice: &ToolChoice) -> AnthropicToolChoice {
    match &choice.mode {
        ToolChoiceMode::Auto => AnthropicToolChoice::Auto {
            disable_parallel_tool_use: choice.disable_parallel_tool_use,
        },
        ToolChoiceMode::None => AnthropicToolChoice::None,
        ToolChoiceMode::RequiredAny => AnthropicToolChoice::Any {
            disable_parallel_tool_use: choice.disable_parallel_tool_use,
        },
        ToolChoiceMode::Specific { tool_name } => AnthropicToolChoice::Tool {
            name: tool_name.clone(),
            disable_parallel_tool_use: choice.disable_parallel_tool_use,
        },
    }
}

fn openai_completions_tool_choice(choice: &ToolChoice) -> OpenAiCompletionsToolChoice {
    match &choice.mode {
        ToolChoiceMode::Auto => OpenAiCompletionsToolChoice::Auto,
        ToolChoiceMode::None => OpenAiCompletionsToolChoice::None,
        ToolChoiceMode::RequiredAny => OpenAiCompletionsToolChoice::Required,
        ToolChoiceMode::Specific { tool_name } => OpenAiCompletionsToolChoice::Function {
            name: tool_name.clone(),
        },
    }
}

fn request_fingerprint(
    model: &ModelSelection,
    kind: &LlmRequestKind,
    run_id: RunId,
    turn_id: TurnId,
) -> Result<String, PlanningError> {
    let encoded = serde_json::to_vec(&(model, kind, run_id, turn_id)).map_err(|error| {
        PlanningError::Rejected(format!("failed to fingerprint request: {error}"))
    })?;
    let digest = Sha256::digest(encoded);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

fn compaction_request_fingerprint(
    model: &ModelSelection,
    kind: &ContextCompactionRequestKind,
) -> Result<String, PlanningError> {
    let encoded = serde_json::to_vec(&(model, kind)).map_err(|error| {
        PlanningError::Rejected(format!("failed to fingerprint compaction request: {error}"))
    })?;
    let digest = Sha256::digest(encoded);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiResponsesRequest {
    pub input_context: ContextSnapshot,
    pub previous_response_id: Option<String>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<OpenAiResponsesToolChoice>,
    pub reasoning: Option<OpenAiReasoningConfig>,
    pub text: Option<Value>,
    pub include: Vec<String>,
    pub max_output_tokens: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub temperature: Option<Value>,
    pub top_p: Option<Value>,
    pub metadata: BTreeMap<String, String>,
    pub parallel_tool_calls: Option<bool>,
    pub store: Option<bool>,
    pub stream: Option<bool>,
    pub truncation: Option<String>,
    pub context_management: Option<Value>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiResponsesToolChoice {
    Auto,
    None,
    Required,
    Function { name: ToolName },
    Raw(Value),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiReasoningConfig {
    pub effort: Option<String>,
    pub summary: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicMessagesRequest {
    pub messages_context: ContextSnapshot,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<AnthropicToolChoice>,
    pub thinking: Option<AnthropicThinkingConfig>,
    pub max_tokens: u32,
    pub metadata: Option<Value>,
    pub stop_sequences: Vec<String>,
    pub stream: Option<bool>,
    pub temperature: Option<Value>,
    pub top_k: Option<u32>,
    pub top_p: Option<Value>,
    pub service_tier: Option<String>,
    pub container: Option<String>,
    pub mcp_servers: Option<Value>,
    pub context_management: Option<Value>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicToolChoice {
    Auto {
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        disable_parallel_tool_use: Option<bool>,
    },
    None,
    Tool {
        name: ToolName,
        disable_parallel_tool_use: Option<bool>,
    },
    Raw(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicThinkingConfig {
    pub r#type: String,
    pub budget_tokens: Option<u32>,
    pub display: Option<String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCompletionsRequest {
    pub messages_context: ContextSnapshot,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<OpenAiCompletionsToolChoice>,
    pub response_format: Option<Value>,
    pub temperature: Option<Value>,
    pub top_p: Option<Value>,
    pub max_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub stop: Option<Value>,
    pub parallel_tool_calls: Option<bool>,
    pub store: Option<bool>,
    pub stream: Option<bool>,
    pub metadata: BTreeMap<String, String>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiCompletionsToolChoice {
    Auto,
    None,
    Required,
    Function { name: ToolName },
    Raw(Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_responses_defaults_include_reusable_reasoning() {
        let defaults = OpenAiResponsesRequestDefaults::default();

        assert_eq!(
            defaults.include,
            vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_owned()]
        );
    }

    #[test]
    fn openai_responses_defaults_deserialize_missing_include_with_reusable_reasoning() {
        let defaults: OpenAiResponsesRequestDefaults = serde_json::from_value(serde_json::json!({
            "reasoning": null,
            "text": null,
            "temperature": null,
            "top_p": null,
            "metadata": {},
            "parallel_tool_calls": null,
            "store": null,
            "stream": null,
            "truncation": null,
            "extra": {}
        }))
        .expect("deserialize defaults");

        assert_eq!(
            defaults.include,
            vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_owned()]
        );
    }

    #[test]
    fn session_config_for_run_applies_generation_overrides() {
        let mut defaults = OpenAiResponsesRequestDefaults::default();
        defaults.reasoning = Some(OpenAiReasoningConfig {
            effort: Some("high".to_owned()),
            summary: Some("auto".to_owned()),
            extra: BTreeMap::new(),
        });
        let config = SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
                options: ModelProviderOptions::None,
            },
            run: RunConfig::default(),
            turn: crate::TurnConfig {
                max_output_tokens: None,
                provider_request_defaults: ProviderRequestDefaults::None,
            },
            context: crate::ContextConfig { compaction: None },
            tools: Default::default(),
        };
        let run_config = RunConfig {
            max_output_tokens: Some(2048),
            provider_request_defaults: Some(ProviderRequestDefaults::OpenAiResponses(
                defaults.clone(),
            )),
            ..RunConfig::default()
        };

        let resolved = session_config_for_run(&config, &run_config);

        assert_eq!(resolved.turn.max_output_tokens, Some(2048));
        assert_eq!(
            resolved.turn.provider_request_defaults,
            ProviderRequestDefaults::OpenAiResponses(defaults)
        );
    }

    #[test]
    fn provider_native_tool_rejects_mismatched_request_api_kind() {
        let mut state = CoreAgentState::new();
        let profile_id = crate::ToolProfileId::new("web");
        let tool_name = ToolName::new("web_search");
        state.tooling.registry.tools.insert(
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
        state.tooling.registry.profiles.insert(
            profile_id.clone(),
            crate::ToolProfile {
                profile_id: profile_id.clone(),
                visible_tools: vec![tool_name],
                tool_choice: None,
            },
        );
        state.tooling.selected_profile_id = Some(profile_id);

        let error = selected_tools_and_choice(&state, &ProviderApiKind::AnthropicMessages)
            .expect_err("provider-native tool must reject mismatched api kind");

        let PlanningError::Domain(DomainError::ProviderCompatibility(message)) = error else {
            panic!("expected provider compatibility error, got {error:?}");
        };
        assert!(message.contains("provider-native tool web_search"));
        assert!(message.contains("OpenAiResponses"));
        assert!(message.contains("AnthropicMessages"));
    }

    #[test]
    fn openai_responses_request_lowers_provider_triggered_compaction() {
        let config = SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
                options: ModelProviderOptions::None,
            },
            run: RunConfig::default(),
            turn: crate::TurnConfig {
                max_output_tokens: None,
                provider_request_defaults: ProviderRequestDefaults::None,
            },
            context: crate::ContextConfig {
                compaction: Some(CompactionPolicy::ProviderTriggered {
                    compact_threshold_tokens: Some(120_000),
                }),
            },
            tools: Default::default(),
        };

        let request = openai_responses_request(
            &config,
            ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries: Vec::new(),
                token_estimate: None,
            },
            Vec::new(),
            None,
            &OpenAiResponsesRequestDefaults::default(),
        );

        assert_eq!(
            request.context_management,
            Some(json!([
                {
                    "type": "compaction",
                    "compact_threshold": 120000
                }
            ]))
        );
    }

    #[test]
    fn openai_responses_request_omits_optional_compact_threshold() {
        let config = SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
                options: ModelProviderOptions::None,
            },
            run: RunConfig::default(),
            turn: crate::TurnConfig {
                max_output_tokens: None,
                provider_request_defaults: ProviderRequestDefaults::None,
            },
            context: crate::ContextConfig {
                compaction: Some(CompactionPolicy::ProviderTriggered {
                    compact_threshold_tokens: None,
                }),
            },
            tools: Default::default(),
        };

        let request = openai_responses_request(
            &config,
            ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries: Vec::new(),
                token_estimate: None,
            },
            Vec::new(),
            None,
            &OpenAiResponsesRequestDefaults::default(),
        );

        assert_eq!(
            request.context_management,
            Some(json!([{ "type": "compaction" }]))
        );
    }
}
