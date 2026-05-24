use serde::{Deserialize, Serialize};

use crate::{
    BlobRef, CoreAgentState, DomainError, ModelProviderOptions, ModelSelection, ProviderApiKind,
    ProviderRequestDefaults, ToolProfileId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub model: ModelSelection,
    pub run: RunConfig,
    pub turn: TurnConfig,
    pub context: ContextConfig,
    pub tool_profile_id: Option<ToolProfileId>,
}

impl SessionConfig {
    pub fn validate_provider_compatibility(&self) -> Result<(), DomainError> {
        validate_model_selection(&self.model)?;
        validate_request_defaults(&self.turn.provider_request_defaults, &self.model.api_kind)?;
        self.run
            .validate_provider_compatibility(&self.model.api_kind)
    }
}

pub(crate) fn validate_config_update_for_state(
    state: &CoreAgentState,
    config: &SessionConfig,
) -> Result<(), DomainError> {
    let current = current_config(state)?;
    config.validate_provider_compatibility()?;
    validate_session_api_kind_is_pinned(&current.model.api_kind, &config.model.api_kind)?;
    validate_active_context_api_kind(state, &config.model.api_kind)?;
    Ok(())
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
    pub instructions_ref: Option<BlobRef>,
    pub max_context_tokens: Option<u32>,
    pub target_context_tokens: Option<u32>,
    pub reserve_output_tokens: Option<u32>,
    pub compaction_enabled: bool,
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

fn current_config(state: &CoreAgentState) -> Result<&SessionConfig, DomainError> {
    state
        .lifecycle
        .config
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("open session is missing config".to_owned()))
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
    if let Some(window) = state.context.active_window.as_ref() {
        if &window.api_kind != api_kind {
            return Err(DomainError::ProviderCompatibility(format!(
                "active context window api kind {:?} does not match session api kind {:?}",
                window.api_kind, api_kind
            )));
        }
    }
    Ok(())
}
