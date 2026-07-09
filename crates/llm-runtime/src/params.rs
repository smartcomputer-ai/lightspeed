//! Typed provider request parameters.
//!
//! The engine carries provider request settings as opaque
//! [`engine::ProviderParams`] (`api_kind` + versioned JSON body). This module
//! owns the typed schemas for those bodies: admission boundaries validate
//! incoming params against them, and adapters parse them when materializing
//! provider-native wire requests. The deterministic core never sees these
//! types.

use std::collections::BTreeMap;

use engine::{ProviderApiKind, ProviderParams};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{LlmAdapterError, LlmAdapterResult};

pub const PROVIDER_PARAMS_VERSION: u32 = 1;

pub const OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE: &str =
    "reasoning.encrypted_content";
pub const OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE: &str = "web_search_call.action.sources";

fn default_openai_responses_include() -> Vec<String> {
    vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_owned()]
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiResponsesParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<OpenAiReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(default = "default_openai_responses_include")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl Default for OpenAiResponsesParams {
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
            max_tool_calls: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiReasoningConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicMessagesParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinkingConfig>,
    /// Output/effort configuration used with adaptive thinking models
    /// (e.g. `{"effort": "high"}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicThinkingConfig {
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCompletionsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// Validate opaque provider params against the typed schema for their API
/// kind. Admission boundaries call this before params enter the session log.
pub fn validate_provider_params(params: &ProviderParams) -> LlmAdapterResult<()> {
    if params.version != PROVIDER_PARAMS_VERSION {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "unsupported provider params version {}, expected {}",
                params.version, PROVIDER_PARAMS_VERSION
            ),
        });
    }
    match params.api_kind {
        ProviderApiKind::OpenAiResponses => {
            parse_params_body::<OpenAiResponsesParams>(&params.body).map(|_| ())
        }
        ProviderApiKind::AnthropicMessages => {
            parse_params_body::<AnthropicMessagesParams>(&params.body).map(|_| ())
        }
        ProviderApiKind::OpenAiCompletions => {
            parse_params_body::<OpenAiCompletionsParams>(&params.body).map(|_| ())
        }
    }
}

/// Parse OpenAI Responses params from optional opaque params, defaulting when
/// absent and rejecting params tagged for a different API kind.
pub fn openai_responses_params(
    params: Option<&ProviderParams>,
) -> LlmAdapterResult<OpenAiResponsesParams> {
    let Some(params) = params else {
        return Ok(OpenAiResponsesParams::default());
    };
    if params.api_kind != ProviderApiKind::OpenAiResponses {
        return Err(LlmAdapterError::RequestKindMismatch {
            message: format!(
                "expected OpenAiResponses provider params, got {:?}",
                params.api_kind
            ),
        });
    }
    if params.version != PROVIDER_PARAMS_VERSION {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "unsupported provider params version {}, expected {}",
                params.version, PROVIDER_PARAMS_VERSION
            ),
        });
    }
    parse_params_body(&params.body)
}

/// Parse Anthropic Messages params from optional opaque params, defaulting
/// when absent and rejecting params tagged for a different API kind.
pub fn anthropic_messages_params(
    params: Option<&ProviderParams>,
) -> LlmAdapterResult<AnthropicMessagesParams> {
    let Some(params) = params else {
        return Ok(AnthropicMessagesParams::default());
    };
    if params.api_kind != ProviderApiKind::AnthropicMessages {
        return Err(LlmAdapterError::RequestKindMismatch {
            message: format!(
                "expected AnthropicMessages provider params, got {:?}",
                params.api_kind
            ),
        });
    }
    if params.version != PROVIDER_PARAMS_VERSION {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "unsupported provider params version {}, expected {}",
                params.version, PROVIDER_PARAMS_VERSION
            ),
        });
    }
    parse_params_body(&params.body)
}

/// Reasoning effort tiers accepted by the OpenAI Responses adapter.
pub const OPENAI_REASONING_EFFORT_TIERS: &[&str] =
    &["none", "minimal", "low", "medium", "high", "xhigh"];

/// Reasoning effort tiers accepted by the Anthropic Messages adapter.
pub const ANTHROPIC_REASONING_EFFORT_TIERS: &[&str] = &["none", "low", "medium", "high", "ultra"];

fn validate_reasoning_effort(
    effort: &str,
    tiers: &'static [&'static str],
    api_kind: ProviderApiKind,
) -> LlmAdapterResult<()> {
    if tiers.contains(&effort) {
        Ok(())
    } else {
        Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "unknown reasoning effort {effort:?} for {api_kind:?}; expected one of {}",
                tiers.join(", ")
            ),
        })
    }
}

/// Materialize an OpenAI Responses reasoning config from an intent effort
/// tier. `"none"` means no reasoning config; other tiers request an effort
/// level with automatic summaries. Unknown tiers are rejected.
pub fn openai_reasoning_from_effort(
    effort: &str,
) -> LlmAdapterResult<Option<OpenAiReasoningConfig>> {
    validate_reasoning_effort(
        effort,
        OPENAI_REASONING_EFFORT_TIERS,
        ProviderApiKind::OpenAiResponses,
    )?;
    if effort == "none" {
        return Ok(None);
    }
    Ok(Some(OpenAiReasoningConfig {
        effort: Some(effort.to_owned()),
        summary: Some("auto".to_owned()),
        extra: BTreeMap::new(),
    }))
}

/// Materialize Anthropic Messages thinking settings from an intent effort
/// tier. Current Anthropic models steer thinking through adaptive thinking
/// plus an `output_config.effort` level, not token budgets. `"none"` means no
/// thinking config; unknown tiers are rejected.
pub fn anthropic_thinking_from_effort(
    effort: &str,
) -> LlmAdapterResult<Option<(AnthropicThinkingConfig, Value)>> {
    validate_reasoning_effort(
        effort,
        ANTHROPIC_REASONING_EFFORT_TIERS,
        ProviderApiKind::AnthropicMessages,
    )?;
    if effort == "none" {
        return Ok(None);
    }
    let thinking = AnthropicThinkingConfig {
        r#type: "adaptive".to_owned(),
        budget_tokens: None,
        display: None,
        extra: BTreeMap::new(),
    };
    Ok(Some((thinking, serde_json::json!({ "effort": effort }))))
}

fn parse_params_body<T: serde::de::DeserializeOwned>(body: &Value) -> LlmAdapterResult<T> {
    serde_json::from_value(body.clone()).map_err(|error| LlmAdapterError::InvalidProviderRequest {
        message: format!("invalid provider params body: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_responses_params_default_include_reusable_reasoning() {
        let params = OpenAiResponsesParams::default();

        assert_eq!(
            params.include,
            vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_owned()]
        );
    }

    #[test]
    fn openai_responses_params_deserialize_missing_include_with_reusable_reasoning() {
        let params: OpenAiResponsesParams =
            serde_json::from_value(json!({ "reasoning": { "effort": "high" } }))
                .expect("deserialize params");

        assert_eq!(
            params.include,
            vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_owned()]
        );
        assert_eq!(
            params.reasoning,
            Some(OpenAiReasoningConfig {
                effort: Some("high".to_owned()),
                summary: None,
                extra: BTreeMap::new(),
            })
        );
    }

    #[test]
    fn validate_provider_params_rejects_unknown_fields() {
        let params = ProviderParams::new(
            ProviderApiKind::OpenAiResponses,
            json!({ "reasonig_effort": "high" }),
        );

        let error = validate_provider_params(&params).expect_err("unknown field must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    #[test]
    fn validate_provider_params_accepts_each_api_kind() {
        for (api_kind, body) in [
            (
                ProviderApiKind::OpenAiResponses,
                json!({ "temperature": 0.2 }),
            ),
            (
                ProviderApiKind::AnthropicMessages,
                json!({ "thinking": { "type": "enabled", "budget_tokens": 2048 } }),
            ),
            (
                ProviderApiKind::OpenAiCompletions,
                json!({ "response_format": { "type": "json_object" } }),
            ),
        ] {
            let params = ProviderParams::new(api_kind, body);
            validate_provider_params(&params).expect("valid params");
        }
    }

    #[test]
    fn openai_reasoning_from_effort_maps_tiers() {
        assert_eq!(
            openai_reasoning_from_effort("none").expect("none tier"),
            None
        );
        for tier in ["minimal", "low", "medium", "high", "xhigh"] {
            let reasoning = openai_reasoning_from_effort(tier)
                .expect("known tier")
                .expect("non-none tier derives reasoning");
            assert_eq!(reasoning.effort.as_deref(), Some(tier));
            assert_eq!(reasoning.summary.as_deref(), Some("auto"));
        }
    }

    #[test]
    fn openai_reasoning_from_effort_rejects_unknown_tier() {
        let error = openai_reasoning_from_effort("ultra").expect_err("unknown tier must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    #[test]
    fn anthropic_thinking_from_effort_maps_tiers() {
        assert_eq!(
            anthropic_thinking_from_effort("none").expect("none tier"),
            None
        );
        for tier in ["low", "medium", "high", "ultra"] {
            let (thinking, output_config) = anthropic_thinking_from_effort(tier)
                .expect("known tier")
                .expect("non-none tier derives thinking");
            assert_eq!(thinking.r#type, "adaptive");
            assert_eq!(thinking.budget_tokens, None);
            assert_eq!(output_config, json!({ "effort": tier }));
        }
    }

    #[test]
    fn anthropic_thinking_from_effort_rejects_unknown_tier() {
        let error = anthropic_thinking_from_effort("xhigh").expect_err("unknown tier must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    #[test]
    fn openai_responses_params_reject_mismatched_api_kind() {
        let params = ProviderParams::new(ProviderApiKind::AnthropicMessages, json!({}));

        let error =
            openai_responses_params(Some(&params)).expect_err("api kind mismatch must fail");
        assert!(matches!(error, LlmAdapterError::RequestKindMismatch { .. }));
    }
}
