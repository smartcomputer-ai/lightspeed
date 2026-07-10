use super::*;

/// Direct provider model discovery. Each invocation asks the supported
/// provider APIs again; clients refresh by calling this method.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    /// Apply Lightspeed's small, conservative selectable-model policy. It
    /// removes OpenAI model-id families that are clearly not text-generation
    /// routes (embeddings, moderation, image/video, speech, and realtime).
    /// It is an ID policy, not a provider capability claim.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub selectable_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitiesView {
    /// Provider-reported effort values. Omitted when the provider does not
    /// report per-model effort support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<String>>,
    /// Omitted when the provider model-list API does not report this fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_use: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u64>,
}

/// A model route returned directly by a provider API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    pub provider_id: String,
    /// Canonical Lightspeed provider API kind, e.g. `openai:responses`.
    pub api_kind: String,
    pub model: String,
    pub display_name: String,
    pub capabilities: ModelCapabilitiesView,
    /// Always `provider` in P97: there is no maintained model catalog.
    pub source: ModelSource,
    pub fetched_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelSource {
    #[default]
    Provider,
}

/// Best-effort status for one provider. A provider failure does not discard
/// models discovered from other providers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderDiscoveryView {
    pub provider_id: String,
    pub api_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at_ms: Option<i64>,
    /// Sanitized Lightspeed error; never a credential, request header, or raw
    /// upstream response body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    #[serde(default)]
    pub models: Vec<ModelView>,
    #[serde(default)]
    pub providers: Vec<ModelProviderDiscoveryView>,
}
