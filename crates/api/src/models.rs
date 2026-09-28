use super::*;

/// Direct provider model discovery. Results may be served from a brief
/// process-local cache; clients refresh by calling this method.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    /// Apply Lightspeed's small, conservative selectable-model policy. It
    /// keeps supported file-transcription routes and filters clearly unrelated
    /// OpenAI families from agent suggestions (embeddings, moderation, image/video,
    /// speech synthesis, and realtime). Agent suggestions also have an age limit.
    /// Clients select routes by API kind for their intended use. This is an ID
    /// policy, not a provider capability claim.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub selectable_only: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitiesView {
    /// Known provider effort values. Prefer provider-reported capabilities;
    /// Lightspeed may enrich important built-in model families when the
    /// provider's model-list API omits this metadata.
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
    /// Provider-reported model creation time. Omitted when the provider does
    /// not expose one; this is distinct from the discovery fetch time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<i64>,
    /// Always `provider`; there is no maintained model catalog.
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
    /// Whether a usable credential exists for this provider — a stable signal
    /// for clients (no string matching on `error`).
    pub credential: ModelProviderCredentialStatus,
    /// Where the credential used for this universe comes from.
    pub credential_source: ModelProviderCredentialSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelProviderCredentialStatus {
    /// A credential was found and was not rejected by the provider (other
    /// discovery errors, e.g. transport, may still be reported).
    Configured,
    /// No credential anywhere: no universe row and no deployment fallback.
    Missing,
    /// A credential exists but is unusable (disabled/legacy universe row) or
    /// was rejected by the provider (HTTP 401/403).
    Invalid,
    /// The universe provider row explicitly configures an unauthenticated
    /// endpoint, so no credential is expected.
    NotRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelProviderCredentialSource {
    /// A universe-owned `model:<provider>` credential.
    Universe,
    /// The deployment-wide fallback key from server configuration.
    Deployment,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    #[serde(default)]
    pub models: Vec<ModelView>,
    #[serde(default)]
    pub providers: Vec<ModelProviderDiscoveryView>,
}
