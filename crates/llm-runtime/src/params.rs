//! Typed provider request parameters.
//!
//! The harness carries provider request settings as opaque
//! [`harness::ProviderParams`] (`api_kind` + versioned JSON body). This module
//! owns the typed schemas for those bodies: admission boundaries validate
//! incoming params against them, and adapters parse them when materializing
//! provider-native wire requests. The deterministic core never sees these
//! types.

use std::collections::BTreeMap;

use harness::{ModelProcessingTier, ProviderApiKind, ProviderParams};
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
    /// OpenAI request processing tier. `fast` is the current name for the
    /// latency-prioritized tier; `priority` remains a provider alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<OpenAiServiceTier>,
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
            service_tier: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiServiceTier {
    Auto,
    Default,
    Flex,
    Fast,
    Priority,
}

impl OpenAiServiceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::Flex => "flex",
            Self::Fast => "fast",
            Self::Priority => "priority",
        }
    }
}

pub(crate) fn take_openai_service_tier(
    extra: &mut BTreeMap<String, Value>,
    tier: Option<OpenAiServiceTier>,
) -> LlmAdapterResult<Option<String>> {
    let legacy = extra.remove("service_tier");
    let typed = tier.map(|tier| tier.as_str().to_owned());
    match (typed, legacy) {
        (Some(typed), Some(Value::String(legacy))) if typed != legacy => {
            Err(LlmAdapterError::InvalidProviderRequest {
                message: "service_tier conflicts with extra.service_tier".to_owned(),
            })
        }
        (Some(typed), Some(Value::String(_)) | None) => Ok(Some(typed)),
        (None, Some(Value::String(legacy))) => Ok(Some(legacy)),
        (_, Some(_)) => Err(LlmAdapterError::InvalidProviderRequest {
            message: "extra.service_tier must be a string".to_owned(),
        }),
        (None, None) => Ok(None),
    }
}

pub(crate) fn openai_processing_service_tier(
    provider_id: &str,
    tier: Option<ModelProcessingTier>,
) -> LlmAdapterResult<Option<String>> {
    let Some(tier) = tier else {
        return Ok(None);
    };
    if provider_id != "openai" {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: "processing tier is supported only by the built-in openai provider".to_owned(),
        });
    }
    Ok(Some(
        match tier {
            ModelProcessingTier::Standard => "default",
            ModelProcessingTier::Fast => "fast",
            ModelProcessingTier::Flex => "flex",
        }
        .to_owned(),
    ))
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
    /// Lifetime of the prompt-cache breakpoints the adapter places on every
    /// request: `"5m"` (default) or `"1h"`. The longer TTL costs more per
    /// cache write and pays off for sessions that wake rarely (bots).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// Prompt-cache TTLs Anthropic accepts on a `cache_control` block.
pub const ANTHROPIC_PROMPT_CACHE_TTLS: [&str; 2] = ["5m", "1h"];

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
    /// OpenAI request processing tier. Compatible providers are not assumed
    /// to support this field; admission exposes it only for built-in OpenAI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<OpenAiServiceTier>,
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
    let parsed: AnthropicMessagesParams = parse_params_body(&params.body)?;
    if let Some(ttl) = parsed.prompt_cache_ttl.as_deref()
        && !ANTHROPIC_PROMPT_CACHE_TTLS.contains(&ttl)
    {
        return Err(LlmAdapterError::InvalidProviderRequest {
            message: format!(
                "unsupported Anthropic prompt_cache_ttl {ttl:?}; expected one of {}",
                ANTHROPIC_PROMPT_CACHE_TTLS.join(", ")
            ),
        });
    }
    Ok(parsed)
}

/// Parse OpenAI Chat Completions params from optional opaque params.
pub fn openai_completions_params(
    params: Option<&ProviderParams>,
) -> LlmAdapterResult<OpenAiCompletionsParams> {
    let Some(params) = params else {
        return Ok(OpenAiCompletionsParams::default());
    };
    if params.api_kind != ProviderApiKind::OpenAiCompletions {
        return Err(LlmAdapterError::RequestKindMismatch {
            message: format!(
                "expected OpenAiCompletions provider params, got {:?}",
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

/// Reasoning effort tiers accepted by current Chat Completions models.
pub const OPENAI_COMPLETIONS_REASONING_EFFORT_TIERS: &[&str] =
    &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Reasoning effort tiers accepted by the Anthropic Messages adapter.
pub const ANTHROPIC_REASONING_EFFORT_TIERS: &[&str] =
    &["none", "low", "medium", "high", "xhigh", "max"];

/// Thinking display mode the adapter requests unless params set one: the
/// summarized reasoning text lands in the session log's reasoning entries.
/// Current models default to `"omitted"`, which returns thinking blocks with
/// an empty `thinking` field and would leave every reasoning entry blank.
pub const ANTHROPIC_THINKING_DISPLAY_SUMMARIZED: &str = "summarized";
/// `thinking.type` that turns thinking off; the API rejects `display` next
/// to it because there is nothing to display.
pub const ANTHROPIC_THINKING_TYPE_DISABLED: &str = "disabled";
/// `thinking.type` that turns thinking off on models that reject
/// `disabled`. It takes no other field.
pub const ANTHROPIC_THINKING_TYPE_BETWEEN_TOOLS: &str = "between_tools";

/// How a Claude model turns thinking off for reasoning effort `"none"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthropicThinkingOff {
    /// `{type: "disabled"}`.
    Disabled,
    /// `{type: "between_tools"}`: the model rejects `disabled` but still
    /// offers a thinking-off mode.
    BetweenTools,
    /// Thinking cannot be turned off; the closest request is adaptive
    /// thinking at the lowest effort.
    LowestEffort,
}

/// Family and version of a Claude model id, e.g. `("opus", 5, 5)` for
/// `claude-opus-5-5`. `None` for ids that are not recognizably Claude, such
/// as models behind Anthropic-compatible endpoints.
fn claude_model_version(model: &str) -> Option<(String, u16, u16)> {
    let normalized = model.to_ascii_lowercase();
    if !normalized.contains("claude") {
        return None;
    }
    let parts = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .collect::<Vec<_>>();
    let family = parts
        .iter()
        .find(|part| matches!(**part, "opus" | "sonnet" | "haiku" | "fable" | "mythos"))?;
    let mut versions = parts.iter().filter_map(|part| {
        let value = part.parse::<u16>().ok()?;
        (value < 100).then_some(value)
    });
    let major = versions.next()?;
    let minor = versions.next().unwrap_or(0);
    Some(((*family).to_owned(), major, minor))
}

/// How `model` turns thinking off. Claude Opus 5.5 and the Fable and Mythos
/// lines keep thinking on at every effort and reject `disabled`; Claude
/// Sonnet 5.5 rejects `disabled` in favour of `between_tools`. Later
/// versions of each line are assumed to keep that behavior; unrecognized
/// ids keep `disabled`.
pub fn anthropic_thinking_off(model: &str) -> AnthropicThinkingOff {
    match claude_model_version(model) {
        Some((family, major, minor)) => match family.as_str() {
            "fable" | "mythos" => AnthropicThinkingOff::LowestEffort,
            "opus" if (major, minor) >= (5, 5) => AnthropicThinkingOff::LowestEffort,
            "sonnet" if (major, minor) >= (5, 5) => AnthropicThinkingOff::BetweenTools,
            _ => AnthropicThinkingOff::Disabled,
        },
        None => AnthropicThinkingOff::Disabled,
    }
}

/// Whether `model` runs adaptive thinking when a request omits `thinking`
/// (the Claude 5 generation and later). Sending `{type: "adaptive"}` to such
/// a model changes nothing except that the request can then carry thinking
/// options such as `block_binding`.
pub fn anthropic_thinks_by_default(model: &str) -> bool {
    match claude_model_version(model) {
        Some((family, major, _)) => match family.as_str() {
            "fable" | "mythos" => true,
            "opus" | "sonnet" => major >= 5,
            _ => false,
        },
        None => false,
    }
}

/// Anthropic thinking settings derived from an intent reasoning effort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnthropicThinkingSettings {
    pub thinking: AnthropicThinkingConfig,
    /// `output_config` carrying the effort level; absent for `"none"`.
    pub output_config: Option<Value>,
}

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

/// Validate and retain a Chat Completions reasoning effort for direct wire
/// forwarding. Model-specific support remains a provider decision.
pub fn validate_openai_reasoning_effort(effort: &str) -> LlmAdapterResult<String> {
    validate_reasoning_effort(
        effort,
        OPENAI_COMPLETIONS_REASONING_EFFORT_TIERS,
        ProviderApiKind::OpenAiCompletions,
    )?;
    Ok(effort.to_owned())
}

/// Materialize Anthropic Messages thinking settings from an intent effort
/// tier. Current Anthropic models steer thinking through adaptive thinking
/// plus an `output_config.effort` level, not token budgets, and the summary
/// text is requested explicitly because those models omit it by default.
/// `"none"` disables thinking explicitly: models such as Claude Opus 5 think
/// whenever a request carries no thinking config, so omitting it would not
/// mean "no reasoning" (models that cannot turn thinking off reject the
/// request, which is the honest outcome for that tier). Unknown tiers are
/// rejected.
pub fn anthropic_thinking_from_effort(
    effort: &str,
    model: &str,
) -> LlmAdapterResult<AnthropicThinkingSettings> {
    validate_reasoning_effort(
        effort,
        ANTHROPIC_REASONING_EFFORT_TIERS,
        ProviderApiKind::AnthropicMessages,
    )?;
    let thinking = |kind: &str, display: Option<&str>| AnthropicThinkingConfig {
        r#type: kind.to_owned(),
        budget_tokens: None,
        display: display.map(str::to_owned),
        extra: BTreeMap::new(),
    };
    if effort == "none" {
        return Ok(match anthropic_thinking_off(model) {
            AnthropicThinkingOff::Disabled => AnthropicThinkingSettings {
                thinking: thinking(ANTHROPIC_THINKING_TYPE_DISABLED, None),
                output_config: None,
            },
            AnthropicThinkingOff::BetweenTools => AnthropicThinkingSettings {
                thinking: thinking(ANTHROPIC_THINKING_TYPE_BETWEEN_TOOLS, None),
                output_config: None,
            },
            // No reasoning was asked for, so none is displayed.
            AnthropicThinkingOff::LowestEffort => AnthropicThinkingSettings {
                thinking: thinking("adaptive", Some("omitted")),
                output_config: Some(serde_json::json!({ "effort": "low" })),
            },
        });
    }
    Ok(AnthropicThinkingSettings {
        thinking: thinking("adaptive", Some(ANTHROPIC_THINKING_DISPLAY_SUMMARIZED)),
        output_config: Some(serde_json::json!({ "effort": effort })),
    })
}

/// What Anthropic does with a replayed thinking block whose conversation
/// prefix no longer matches the one it was produced in.
///
/// Repairs that rewrite content the provider has already seen (image
/// normalization of an existing history, media omission, redaction) change
/// that prefix. `DropBlock` lets the session continue without the affected
/// reasoning; `Error` fails the request, which suites use to catch
/// unintended history edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThinkingPrefixMismatch {
    #[default]
    DropBlock,
    Error,
}

impl ThinkingPrefixMismatch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DropBlock => "drop_block",
            Self::Error => "error",
        }
    }
}

impl ThinkingPrefixMismatch {
    /// The `thinking.block_binding` object carrying this behavior.
    pub fn block_binding(self) -> Value {
        serde_json::json!({ "prefix_mismatch_behavior": self.as_str() })
    }
}

/// Thinking types that turn thinking on. The API accepts `display` and
/// `block_binding` only next to these; `disabled` and `between_tools` reject
/// both.
fn thinking_is_on(thinking: &AnthropicThinkingConfig) -> bool {
    matches!(thinking.r#type.as_str(), "adaptive" | "enabled")
}

/// Fill in `block_binding` when params leave it unset and thinking is on.
/// Explicit params keep their value.
pub fn default_anthropic_block_binding(
    thinking: &mut AnthropicThinkingConfig,
    behavior: ThinkingPrefixMismatch,
) {
    if thinking_is_on(thinking) && !thinking.extra.contains_key("block_binding") {
        thinking
            .extra
            .insert("block_binding".to_owned(), behavior.block_binding());
    }
}

/// Fill in the thinking display mode when params leave it unset so reasoning
/// entries carry summary text. Explicit params keep their value; thinking
/// that is off never gets one.
pub fn default_anthropic_thinking_display(thinking: &mut AnthropicThinkingConfig) {
    if thinking.display.is_none() && thinking_is_on(thinking) {
        thinking.display = Some(ANTHROPIC_THINKING_DISPLAY_SUMMARIZED.to_owned());
    }
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
    fn openai_service_tiers_are_typed_for_both_api_kinds() {
        for api_kind in [
            ProviderApiKind::OpenAiResponses,
            ProviderApiKind::OpenAiCompletions,
        ] {
            for tier in ["auto", "default", "flex", "fast", "priority"] {
                validate_provider_params(&ProviderParams::new(
                    api_kind.clone(),
                    json!({"service_tier": tier}),
                ))
                .expect("documented service tier");
            }
            assert!(
                validate_provider_params(&ProviderParams::new(
                    api_kind,
                    json!({"service_tier": "turbo"}),
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn typed_service_tier_rejects_conflicting_legacy_extra_value() {
        let mut extra = BTreeMap::from([("service_tier".to_owned(), json!("flex"))]);
        let error = take_openai_service_tier(&mut extra, Some(OpenAiServiceTier::Fast))
            .expect_err("conflicting tier");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    #[test]
    fn session_processing_tiers_lower_to_openai_service_tiers() {
        for (tier, expected) in [
            (ModelProcessingTier::Standard, "default"),
            (ModelProcessingTier::Fast, "fast"),
            (ModelProcessingTier::Flex, "flex"),
        ] {
            assert_eq!(
                openai_processing_service_tier("openai", Some(tier)).expect("OpenAI tier"),
                Some(expected.to_owned())
            );
        }
        assert!(
            openai_processing_service_tier("openrouter", Some(ModelProcessingTier::Fast)).is_err()
        );
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
        let none = anthropic_thinking_from_effort("none", "claude-opus-4-8").expect("none tier");
        assert_eq!(none.thinking.r#type, "disabled");
        assert_eq!(none.thinking.display, None);
        assert_eq!(none.output_config, None);
        for tier in ["low", "medium", "high", "xhigh", "max"] {
            let settings =
                anthropic_thinking_from_effort(tier, "claude-opus-5-5").expect("known tier");
            assert_eq!(settings.thinking.r#type, "adaptive");
            assert_eq!(settings.thinking.budget_tokens, None);
            assert_eq!(settings.thinking.display.as_deref(), Some("summarized"));
            assert_eq!(settings.output_config, Some(json!({ "effort": tier })));
        }
    }

    #[test]
    fn none_effort_uses_each_models_thinking_off_mode() {
        for model in [
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-sonnet-5",
            "custom-model",
        ] {
            let none = anthropic_thinking_from_effort("none", model).expect("none tier");
            assert_eq!(none.thinking.r#type, "disabled", "{model}");
            assert_eq!(none.output_config, None, "{model}");
        }

        let none = anthropic_thinking_from_effort("none", "claude-sonnet-5-5").expect("none tier");
        assert_eq!(none.thinking.r#type, "between_tools");
        assert_eq!(none.thinking.display, None);
        assert_eq!(none.output_config, None);

        for model in [
            "claude-opus-5-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "anthropic.claude-opus-5-5",
        ] {
            let none = anthropic_thinking_from_effort("none", model).expect("none tier");
            assert_eq!(none.thinking.r#type, "adaptive", "{model}");
            assert_eq!(none.thinking.display.as_deref(), Some("omitted"), "{model}");
            assert_eq!(
                none.output_config,
                Some(json!({ "effort": "low" })),
                "{model}"
            );
        }
    }

    #[test]
    fn claude_5_models_think_by_default() {
        for model in [
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-sonnet-5",
            "claude-sonnet-5-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
        ] {
            assert!(anthropic_thinks_by_default(model), "{model}");
        }
        for model in [
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-3-7-sonnet-20250219",
            "custom-anthropic-compatible",
        ] {
            assert!(!anthropic_thinks_by_default(model), "{model}");
        }
    }

    #[test]
    fn anthropic_thinking_from_effort_rejects_unknown_tier() {
        let error = anthropic_thinking_from_effort("ultra", "claude-opus-5-5")
            .expect_err("unknown tier must fail");
        assert!(matches!(
            error,
            LlmAdapterError::InvalidProviderRequest { .. }
        ));
    }

    #[test]
    fn default_anthropic_thinking_display_fills_only_unset_enabled_modes() {
        let mut adaptive = AnthropicThinkingConfig {
            r#type: "adaptive".to_owned(),
            budget_tokens: None,
            display: None,
            extra: BTreeMap::new(),
        };
        default_anthropic_thinking_display(&mut adaptive);
        assert_eq!(adaptive.display.as_deref(), Some("summarized"));

        let mut explicit = AnthropicThinkingConfig {
            r#type: "enabled".to_owned(),
            budget_tokens: Some(1024),
            display: Some("omitted".to_owned()),
            extra: BTreeMap::new(),
        };
        default_anthropic_thinking_display(&mut explicit);
        assert_eq!(explicit.display.as_deref(), Some("omitted"));

        let mut disabled = AnthropicThinkingConfig {
            r#type: "disabled".to_owned(),
            budget_tokens: None,
            display: None,
            extra: BTreeMap::new(),
        };
        default_anthropic_thinking_display(&mut disabled);
        assert_eq!(disabled.display, None);

        let mut between_tools = AnthropicThinkingConfig {
            r#type: "between_tools".to_owned(),
            budget_tokens: None,
            display: None,
            extra: BTreeMap::new(),
        };
        default_anthropic_thinking_display(&mut between_tools);
        assert_eq!(between_tools.display, None);
    }

    #[test]
    fn default_anthropic_block_binding_fills_only_unset_thinking_on_modes() {
        let thinking = |kind: &str| AnthropicThinkingConfig {
            r#type: kind.to_owned(),
            budget_tokens: None,
            display: None,
            extra: BTreeMap::new(),
        };
        for kind in ["adaptive", "enabled"] {
            let mut config = thinking(kind);
            default_anthropic_block_binding(&mut config, ThinkingPrefixMismatch::DropBlock);
            assert_eq!(
                config.extra.get("block_binding"),
                Some(&json!({ "prefix_mismatch_behavior": "drop_block" }))
            );
        }
        for kind in ["disabled", "between_tools"] {
            let mut config = thinking(kind);
            default_anthropic_block_binding(&mut config, ThinkingPrefixMismatch::DropBlock);
            assert!(config.extra.is_empty(), "{kind} rejects block_binding");
        }

        let mut explicit = thinking("adaptive");
        explicit.extra.insert(
            "block_binding".to_owned(),
            json!({ "prefix_mismatch_behavior": "error" }),
        );
        default_anthropic_block_binding(&mut explicit, ThinkingPrefixMismatch::DropBlock);
        assert_eq!(
            explicit.extra.get("block_binding"),
            Some(&json!({ "prefix_mismatch_behavior": "error" }))
        );
    }

    #[test]
    fn openai_responses_params_reject_mismatched_api_kind() {
        let params = ProviderParams::new(ProviderApiKind::AnthropicMessages, json!({}));

        let error =
            openai_responses_params(Some(&params)).expect_err("api kind mismatch must fail");
        assert!(matches!(error, LlmAdapterError::RequestKindMismatch { .. }));
    }
}
