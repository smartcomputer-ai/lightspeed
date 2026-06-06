use serde::{Deserialize, Serialize};

use crate::{
    CoreAgentState, DomainError, ModelProviderOptions, ModelSelection, ProviderApiKind,
    ProviderRequestDefaults,
};

const MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD: u32 = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub model: ModelSelection,
    pub run: RunConfig,
    pub turn: TurnConfig,
    pub context: ContextConfig,
    #[serde(default)]
    pub tools: ToolConfig,
}

impl SessionConfig {
    pub fn validate_provider_compatibility(&self) -> Result<(), DomainError> {
        validate_model_selection(&self.model)?;
        validate_request_defaults(&self.turn.provider_request_defaults, &self.model.api_kind)?;
        validate_context_config(&self.context, &self.model.api_kind)?;
        validate_tool_config(&self.tools, &self.model.api_kind)?;
        self.run
            .validate_provider_compatibility(&self.model.api_kind)
    }
}

pub(crate) fn validate_config_update_for_state(
    state: &CoreAgentState,
    config: &SessionConfig,
) -> Result<(), DomainError> {
    let current = current_config(state)?;
    validate_session_is_idle_for_config_update(state)?;
    config.validate_provider_compatibility()?;
    validate_session_api_kind_is_pinned(&current.model.api_kind, &config.model.api_kind)?;
    validate_active_context_api_kind(state, &config.model.api_kind)?;
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "RunConfigPatch::is_empty")]
    pub run: RunConfigPatch,
    #[serde(default, skip_serializing_if = "TurnConfigPatch::is_empty")]
    pub turn: TurnConfigPatch,
    #[serde(default, skip_serializing_if = "ContextConfigPatch::is_empty")]
    pub context: ContextConfigPatch,
    #[serde(default, skip_serializing_if = "ToolConfigPatch::is_empty")]
    pub tools: ToolConfigPatch,
}

impl SessionConfigPatch {
    pub fn apply_to(&self, config: &SessionConfig) -> SessionConfig {
        let mut next = config.clone();
        if let Some(model) = self.model.clone() {
            next.model = model;
        }
        self.run.apply_to(&mut next.run);
        self.turn.apply_to(&mut next.turn);
        self.context.apply_to(&mut next.context);
        self.tools.apply_to(&mut next.tools);
        next
    }

    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.run.is_empty()
            && self.turn.is_empty()
            && self.context.is_empty()
            && self.tools.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "value")]
pub enum OptionalConfigPatch<T> {
    Set(T),
    Clear,
}

impl<T: Clone> OptionalConfigPatch<T> {
    pub fn apply_to(&self, value: &mut Option<T>) {
        match self {
            Self::Set(next) => *value = Some(next.clone()),
            Self::Clear => *value = None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<OptionalConfigPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<OptionalConfigPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<OptionalConfigPatch<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<OptionalConfigPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_defaults: Option<OptionalConfigPatch<ProviderRequestDefaults>>,
}

impl RunConfigPatch {
    pub fn apply_to(&self, config: &mut RunConfig) {
        apply_optional_config_patch(&mut config.max_turns, &self.max_turns);
        apply_optional_config_patch(&mut config.max_tool_rounds, &self.max_tool_rounds);
        apply_optional_config_patch(&mut config.model_override, &self.model_override);
        apply_optional_config_patch(&mut config.max_output_tokens, &self.max_output_tokens);
        apply_optional_config_patch(
            &mut config.provider_request_defaults,
            &self.provider_request_defaults,
        );
    }

    pub fn is_empty(&self) -> bool {
        self.max_turns.is_none()
            && self.max_tool_rounds.is_none()
            && self.model_override.is_none()
            && self.max_output_tokens.is_none()
            && self.provider_request_defaults.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<OptionalConfigPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_defaults: Option<ProviderRequestDefaults>,
}

impl TurnConfigPatch {
    pub fn apply_to(&self, config: &mut TurnConfig) {
        apply_optional_config_patch(&mut config.max_output_tokens, &self.max_output_tokens);
        if let Some(defaults) = self.provider_request_defaults.clone() {
            config.provider_request_defaults = defaults;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.max_output_tokens.is_none() && self.provider_request_defaults.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<OptionalConfigPatch<CompactionPolicy>>,
}

impl ContextConfigPatch {
    pub fn apply_to(&self, config: &mut ContextConfig) {
        apply_optional_config_patch(&mut config.compaction, &self.compaction);
    }

    pub fn is_empty(&self) -> bool {
        self.compaction.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<OptionalConfigPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<OptionalConfigPatch<HostToolMode>>,
}

impl ToolConfigPatch {
    pub fn apply_to(&self, config: &mut ToolConfig) {
        apply_optional_config_patch(&mut config.web_search, &self.web_search);
        apply_optional_config_patch(&mut config.host, &self.host);
    }

    pub fn is_empty(&self) -> bool {
        self.web_search.is_none() && self.host.is_none()
    }
}

fn apply_optional_config_patch<T: Clone>(
    value: &mut Option<T>,
    patch: &Option<OptionalConfigPatch<T>>,
) {
    if let Some(patch) = patch {
        patch.apply_to(value);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_defaults: Option<ProviderRequestDefaults>,
}

impl RunConfig {
    pub fn validate_provider_compatibility(
        &self,
        session_api_kind: &ProviderApiKind,
    ) -> Result<(), DomainError> {
        let api_kind = if let Some(model) = self.model_override.as_ref() {
            validate_model_selection(model)?;
            if &model.api_kind != session_api_kind {
                return Err(DomainError::ProviderCompatibility(format!(
                    "run model override api kind {:?} does not match session api kind {:?}",
                    model.api_kind, session_api_kind
                )));
            }
            &model.api_kind
        } else {
            session_api_kind
        };
        if let Some(defaults) = self.provider_request_defaults.as_ref() {
            validate_request_defaults(defaults, api_kind)?;
        }
        Ok(())
    }
}

pub(crate) fn validate_run_config_for_state(
    state: &CoreAgentState,
    run_config: &RunConfig,
) -> Result<(), DomainError> {
    let config = current_config(state)?;
    run_config.validate_provider_compatibility(&config.model.api_kind)?;
    validate_active_context_api_kind(state, &config.model.api_kind)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnConfig {
    pub max_output_tokens: Option<u32>,
    pub provider_request_defaults: ProviderRequestDefaults,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextConfig {
    pub compaction: Option<CompactionPolicy>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostToolMode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostToolMode {
    None,
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum CompactionPolicy {
    Disabled,
    ProviderTriggered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_threshold_tokens: Option<u32>,
    },
    ProviderStandalone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_threshold_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_tokens: Option<u32>,
    },
}

fn validate_model_selection(model: &ModelSelection) -> Result<(), DomainError> {
    match (&model.api_kind, &model.options) {
        (_, ModelProviderOptions::None)
        | (ProviderApiKind::OpenAiResponses, ModelProviderOptions::OpenAiResponses(_))
        | (ProviderApiKind::AnthropicMessages, ModelProviderOptions::AnthropicMessages(_))
        | (ProviderApiKind::OpenAiCompletions, ModelProviderOptions::OpenAiCompletions(_)) => {
            Ok(())
        }
        (api_kind, options) => Err(DomainError::ProviderCompatibility(format!(
            "model options {:?} do not match provider api kind {:?}",
            options, api_kind
        ))),
    }
}

fn validate_request_defaults(
    defaults: &ProviderRequestDefaults,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    match (api_kind, defaults) {
        (_, ProviderRequestDefaults::None)
        | (ProviderApiKind::OpenAiResponses, ProviderRequestDefaults::OpenAiResponses(_))
        | (ProviderApiKind::AnthropicMessages, ProviderRequestDefaults::AnthropicMessages(_))
        | (ProviderApiKind::OpenAiCompletions, ProviderRequestDefaults::OpenAiCompletions(_)) => {
            Ok(())
        }
        (api_kind, defaults) => Err(DomainError::ProviderCompatibility(format!(
            "request defaults {:?} do not match provider api kind {:?}",
            defaults, api_kind
        ))),
    }
}

fn validate_context_config(
    context: &ContextConfig,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    match (&context.compaction, api_kind) {
        (None | Some(CompactionPolicy::Disabled), _) => Ok(()),
        (
            Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens,
            }),
            ProviderApiKind::OpenAiResponses,
        ) => validate_openai_responses_compact_threshold(*compact_threshold_tokens),
        (
            Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens,
                target_tokens,
            }),
            ProviderApiKind::OpenAiResponses,
        ) => validate_provider_standalone_compaction(*compact_threshold_tokens, *target_tokens),
        (Some(CompactionPolicy::ProviderTriggered { .. }), api_kind) => {
            Err(DomainError::ProviderCompatibility(format!(
                "provider-triggered compaction requires OpenAI Responses api kind, got {:?}",
                api_kind
            )))
        }
        (Some(CompactionPolicy::ProviderStandalone { .. }), api_kind) => {
            Err(DomainError::ProviderCompatibility(format!(
                "provider-standalone compaction requires OpenAI Responses api kind, got {:?}",
                api_kind
            )))
        }
    }
}

fn validate_tool_config(tools: &ToolConfig, api_kind: &ProviderApiKind) -> Result<(), DomainError> {
    if tools.web_search == Some(true) && api_kind != &ProviderApiKind::OpenAiResponses {
        return Err(DomainError::ProviderCompatibility(format!(
            "web search requires OpenAI Responses api kind, got {:?}",
            api_kind
        )));
    }
    Ok(())
}

fn validate_openai_responses_compact_threshold(
    compact_threshold_tokens: Option<u32>,
) -> Result<(), DomainError> {
    if compact_threshold_tokens
        .is_some_and(|threshold| threshold < MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD)
    {
        return Err(DomainError::ProviderCompatibility(format!(
            "OpenAI Responses compact_threshold_tokens must be at least {} when set",
            MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD
        )));
    }
    Ok(())
}

fn validate_provider_standalone_compaction(
    compact_threshold_tokens: Option<u32>,
    target_tokens: Option<u32>,
) -> Result<(), DomainError> {
    if compact_threshold_tokens.is_some_and(|tokens| tokens == 0) {
        return Err(DomainError::ProviderCompatibility(
            "provider-standalone compaction compact_threshold_tokens must be greater than 0 when set"
                .to_owned(),
        ));
    }
    if target_tokens.is_some_and(|tokens| tokens == 0) {
        return Err(DomainError::ProviderCompatibility(
            "provider-standalone compaction target_tokens must be greater than 0 when set"
                .to_owned(),
        ));
    }
    Ok(())
}

fn current_config(state: &CoreAgentState) -> Result<&SessionConfig, DomainError> {
    state
        .lifecycle
        .config
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("open session is missing config".to_owned()))
}

fn validate_session_is_idle_for_config_update(state: &CoreAgentState) -> Result<(), DomainError> {
    if state.runs.active.is_some() || !state.runs.queued.is_empty() {
        Err(DomainError::InvariantViolation(
            "session config can only change while no run is active or queued".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_session_api_kind_is_pinned(
    pinned: &ProviderApiKind,
    proposed: &ProviderApiKind,
) -> Result<(), DomainError> {
    if proposed == pinned {
        Ok(())
    } else {
        Err(DomainError::ProviderCompatibility(format!(
            "session provider api kind is pinned to {:?}, got {:?}",
            pinned, proposed
        )))
    }
}

fn validate_active_context_api_kind(
    state: &CoreAgentState,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    let _ = (state, api_kind);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(api_kind: ProviderApiKind, compaction: Option<CompactionPolicy>) -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind,
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
                options: ModelProviderOptions::None,
            },
            run: RunConfig::default(),
            turn: TurnConfig {
                max_output_tokens: None,
                provider_request_defaults: ProviderRequestDefaults::None,
            },
            context: ContextConfig { compaction },
            tools: Default::default(),
        }
    }

    #[test]
    fn provider_triggered_compaction_rejects_too_small_openai_threshold() {
        let config = config(
            ProviderApiKind::OpenAiResponses,
            Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens: Some(999),
            }),
        );

        let error = config
            .validate_provider_compatibility()
            .expect_err("threshold below provider minimum must fail");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn provider_triggered_compaction_accepts_optional_or_minimum_openai_threshold() {
        for compact_threshold_tokens in [None, Some(MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD)] {
            let config = config(
                ProviderApiKind::OpenAiResponses,
                Some(CompactionPolicy::ProviderTriggered {
                    compact_threshold_tokens,
                }),
            );

            config
                .validate_provider_compatibility()
                .expect("valid OpenAI provider-triggered compaction");
        }
    }

    #[test]
    fn provider_triggered_compaction_rejects_non_openai_responses_api_kind() {
        let config = config(
            ProviderApiKind::AnthropicMessages,
            Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens: None,
            }),
        );

        let error = config
            .validate_provider_compatibility()
            .expect_err("provider-triggered compaction is OpenAI Responses only");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn provider_standalone_compaction_rejects_zero_values() {
        for compaction in [
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: Some(0),
                target_tokens: Some(128),
            },
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: Some(128),
                target_tokens: Some(0),
            },
        ] {
            let config = config(ProviderApiKind::OpenAiResponses, Some(compaction));

            let error = config
                .validate_provider_compatibility()
                .expect_err("zero standalone compaction values must fail");

            assert!(matches!(error, DomainError::ProviderCompatibility(_)));
        }
    }

    #[test]
    fn provider_standalone_compaction_accepts_optional_or_positive_values() {
        for compaction in [
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: None,
                target_tokens: None,
            },
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: Some(1),
                target_tokens: Some(1),
            },
        ] {
            let config = config(ProviderApiKind::OpenAiResponses, Some(compaction));

            config
                .validate_provider_compatibility()
                .expect("valid OpenAI provider-standalone compaction");
        }
    }

    #[test]
    fn provider_standalone_compaction_rejects_non_openai_responses_api_kind() {
        let config = config(
            ProviderApiKind::AnthropicMessages,
            Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: None,
                target_tokens: None,
            }),
        );

        let error = config
            .validate_provider_compatibility()
            .expect_err("provider-standalone compaction is OpenAI Responses only");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn web_search_enable_requires_openai_responses() {
        let mut config = config(ProviderApiKind::AnthropicMessages, None);
        config.tools.web_search = Some(true);

        let error = config
            .validate_provider_compatibility()
            .expect_err("web search should reject Anthropic");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }
}
