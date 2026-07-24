use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileSource>,
}

/// Current version of every feature block. Omitted versions on input decode
/// to this value; documents read back from a session always carry the pinned
/// version.
pub const CURRENT_FEATURE_VERSION: u32 = 1;

fn default_feature_version() -> u32 {
    CURRENT_FEATURE_VERSION
}

/// Declared session configuration document.
///
/// Sparse and capability-oriented: an omitted section means defaults, an
/// absent feature is not granted (no tools, no access). The document is
/// replaced whole via `session/config/put`; reads return exactly the stored
/// document, so read-modify-write round-trips.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionConfig {
    /// Absent on input means the deployment default model. Documents read
    /// back from a session always carry the model; the provider api kind is
    /// pinned for the session's lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,
}

/// Turn-shaping defaults applied to every LLM generation. Per-run overrides
/// ride `session/runs/start`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Reasoning effort tier as a provider-native string (e.g. "none",
    /// "high", "xhigh", "max"); validated against the session's provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether the model may call several tools in one turn; absent leaves
    /// the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_use: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolChoice {
    Auto,
    None,
    RequiredAny,
    Specific { tool_id: String },
}

/// Run budget defaults enforced by the engine drive loop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionPolicy>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "mode")]
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

/// Capability grants. An absent feature is not granted; `{}` grants it with
/// defaults. Every block carries a behavior `version` that pins semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeaturesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs: Option<VfsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<MessagingFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<FleetFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timers: Option<TimersFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<EnvironmentsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpFeature>,
}

/// Grants the session virtual filesystem: mounts may be attached and the VFS
/// catalog is surfaced. Sub-grants are independent; `{}` grants a VFS with
/// no tools and no sourcing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VfsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Agent-facing filesystem tool surface; absent = no fs tools. Per-path
    /// writability is defined by each mount's own access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<VfsToolSurface>,
    /// Prompt-instruction sourcing from the VFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<VfsPromptsConfig>,
    /// Skill discovery sourcing from the VFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<VfsSkillsConfig>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VfsToolSurface {
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VfsPromptsConfig {
    /// Absent means the conventional roots; an explicit list must be
    /// non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VfsSkillsConfig {
    /// Absent means the conventional roots; an explicit list must be
    /// non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

/// Grants network access through the web toolset; `fetch` and `search` are
/// independently granted, and a web block granting neither is rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<WebFetchFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchFeature>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFetchFeature {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchFeature {
    /// Absent means all domains; an explicit list must be non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
}

/// Grants the messaging toolset (message_send/react/edit/noop) for sessions
/// bound to a chat channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessagingFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
}

/// Grants the Fleet subagent control plane
/// (agent_spawn/send/read/list/cancel and profile_list/read).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<FleetProfilesConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<FleetSpawnConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetProfilesConfig {
    /// Absent means all named profiles are visible/readable/spawnable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<ProfileId>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<ProfileId>,
    /// Defaults to true when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FleetSpawnConfig {
    /// Absent means all bases are allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bases: Option<Vec<FleetSpawnBase>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FleetSpawnBase {
    #[serde(rename = "self")]
    Self_,
    Session,
    Profile,
}

/// Grants timer promises through the sleep tool plus the base concurrency
/// tools (await/cancel/detach).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimersFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
}

/// Grants attaching/activating session environments and their process/job
/// tool surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Absent means every registered provider is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<EnvironmentProviderId>>,
}

/// Grants remote MCP tools by declaring linked servers from the universe MCP
/// catalog; must link at least one server, with unique server ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<McpServerLink>,
}

/// A linked catalog server with optional per-session overrides; absent
/// fields defer to the catalog record's defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerLink {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<RemoteMcpApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    /// Universe-scoped auth grant used to authenticate against the server;
    /// compatibility with the server's auth policy is validated at put time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_grant_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResponse {
    pub session: SessionView,
}

/// Replace the session config with a complete document. Anything omitted
/// from the document reverts to defaults; an absent feature is revoked.
/// Requires an idle session; putting an identical document is a no-op that
/// leaves the revision untouched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigPutParams {
    pub session_id: SessionId,
    /// Checked against the session's current config revision when present;
    /// absent replaces unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_config_revision: Option<u64>,
    pub config: SessionConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigPutResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsView {
    pub revision: u64,
    #[serde(default)]
    pub tools: Vec<ToolView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    pub tool_id: String,
    pub kind: ToolKindView,
    #[serde(default)]
    pub parallelism: ToolParallelismView,
    #[serde(default)]
    pub target_requirement: ToolTargetRequirementView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolKindView {
    Function {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        input_schema_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options_ref: Option<String>,
    },
    ProviderNative {
        api_kind: String,
        native_tool_ref: String,
        execution: ProviderNativeToolExecutionView,
    },
    RemoteMcp {
        server_label: String,
        server_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_tools: Option<Vec<String>>,
        #[serde(default)]
        approval: RemoteMcpApprovalPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_ref: Option<SecretRefView>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProviderNativeToolExecutionView {
    #[default]
    ProviderHosted,
    ClientEffect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolParallelismView {
    Exclusive,
    #[default]
    ParallelSafe,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolTargetRequirementView {
    #[default]
    None,
    Optional {
        namespace: String,
    },
    Required {
        namespace: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextCompactParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextCompactResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendParams {
    pub session_id: SessionId,
    pub entries: Vec<ContextAppendEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendEntry {
    /// Stable client-chosen context key. Re-sending the same key with the
    /// same content is a no-op, so the key doubles as the idempotency handle.
    pub key: String,
    pub item: InputItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendResponse {
    pub context_revision: u64,
    pub results: Vec<ContextAppendResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendResult {
    pub key: String,
    pub status: ContextAppendStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<ContextEntryInputView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<InputAdmissionFailureView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_text: Option<String>,
    /// True when `activation_text` was cut off at the server-side length cap.
    /// The committed context entry always holds the full text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub activation_text_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextAppendStatus {
    Applied,
    Unchanged,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InputAdmissionFailureView {
    pub kind: InputAdmissionFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InputAdmissionFailureKind {
    UnsupportedMedia,
    UnsupportedAudioMime,
    BlobMissing,
    BlobTooLarge,
    AudioDurationTooLong,
    TranscoderUnavailable,
    TranscodeFailure,
    TranscriptionFailure,
    AdmissionRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRemoveParams {
    pub session_id: SessionId,
    /// Active context keys to remove. Removing a key that is already absent
    /// is a per-key no-op (`absent`), so retries are idempotent. Keys under
    /// reserved runtime namespaces (`run.`) are rejected request-level.
    pub keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRemoveResponse {
    pub context_revision: u64,
    pub results: Vec<ContextRemoveResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRemoveResult {
    pub key: String,
    pub status: ContextRemoveStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<InputAdmissionFailureView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextRemoveStatus {
    Removed,
    Absent,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxReadParams {
    /// Return pending entries with `seq` greater than this cursor. Restart
    /// from 0 to re-read undelivered entries after a consumer restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Long-poll wait in milliseconds when no entries are pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxReadResponse {
    pub entries: Vec<OutboundMessageView>,
    /// Cursor to pass as `after` on the next read.
    pub next_after: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboundMessageView {
    pub seq: u64,
    pub outbox_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub origin: OutboundOriginView,
    pub payload: OutboundPayloadView,
    pub attempts: u32,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OutboundOriginView {
    ToolCall,
    FinalText,
    Trigger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OutboundPayloadView {
    Send {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
    },
    React {
        message_id: String,
        emoji: String,
    },
    Edit {
        message_id: String,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxAckParams {
    pub outbox_id: String,
    pub result: OutboundAckInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OutboundAckInput {
    Delivered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel_message_id: Option<String>,
    },
    Failed {
        error: String,
        retryable: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxAckResponse {
    pub outbox_id: String,
    pub status: OutboundStatusView,
    pub attempts: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OutboundStatusView {
    Pending,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListParams {
    /// Opaque cursor from the previous page's `nextCursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResponse {
    #[serde(default)]
    pub sessions: Vec<SessionSummaryView>,
    /// Present when more sessions exist past this page. Ordering is most
    /// recently updated first; pages can drift under concurrent activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummaryView {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub lifecycle_status: SessionLifecycleStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionLifecycleStatus {
    New,
    Open,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameParams {
    pub session_id: SessionId,
    /// New display name; absent clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameResponse {
    pub session: SessionSummaryView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteResponse {
    pub session: SessionSummaryView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Long-poll: when no events exist past `after`, hold the request until
    /// one lands or this many milliseconds elapse, then return a normal
    /// (possibly empty) page. Zero or absent preserves immediate return.
    /// Values above the server cap are clamped, not rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadResponse {
    #[serde(default)]
    pub events: Vec<SessionEventView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_cursor: Option<EventCursor>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<EventLogGap>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseParams {
    pub session_id: SessionId,
    /// Cancel the active run and drop queued runs instead of rejecting on
    /// active work. Recovers sessions whose workflow no longer exists (e.g.
    /// after an operator terminate) by reconciling the session log directly.
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseResponse {
    pub session: SessionView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventLogGap {
    pub requested_after: Option<EventCursor>,
    pub retained_after: Option<EventCursor>,
    pub next_cursor: Option<EventCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventView {
    pub cursor: EventCursor,
    pub session_id: SessionId,
    pub observed_at_ms: u64,
    pub joins: EventJoinsView,
    pub kind: SessionEventKindView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventJoinsView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventKindView {
    SessionOpened {
        model: Option<ModelConfig>,
    },
    SessionConfigChanged {
        model: Option<ModelConfig>,
        revision: u64,
    },
    WorkflowControllerPortsConfigured {
        controller_workflow_kind: String,
        creation_fingerprint: String,
        port_ids: Vec<String>,
    },
    SessionClosed,
    RunAccepted {
        run_id: RunId,
        submission_id: Option<String>,
        source: RunAcceptedSourceView,
    },
    RunStarted {
        run_id: RunId,
    },
    MessageBuffered {
        message_id: String,
        submission_id: Option<String>,
    },
    MessageConsumedByAwait {
        message_id: String,
        run_id: RunId,
    },
    MessagePromotedToRun {
        message_id: String,
        run_id: RunId,
    },
    MessageCancelled {
        message_id: String,
    },
    RunSteeringAccepted {
        run_id: RunId,
        steering_id: String,
        input: Vec<ContextEntryInputView>,
    },
    RunCancellationRequested {
        run_id: RunId,
    },
    RunCompleted {
        run_id: RunId,
        output_ref: Option<String>,
    },
    RunFailed {
        run_id: RunId,
        message: String,
    },
    RunCancelled {
        run_id: RunId,
    },
    PromiseCreated {
        promise_id: String,
        source: String,
    },
    PromiseResolved {
        promise_id: String,
        payload_ref: Option<String>,
    },
    PromiseFailed {
        promise_id: String,
        error_ref: Option<String>,
    },
    PromiseCancelled {
        promise_id: String,
    },
    PromiseDetached {
        promise_id: String,
    },
    TurnStarted {
        run_id: RunId,
        turn_id: String,
    },
    TurnPlanned {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationRequested {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationCompleted {
        run_id: RunId,
        turn_id: String,
        status: String,
    },
    TurnCompleted {
        turn_id: String,
    },
    ContextEntriesApplied {
        base_revision: u64,
        revision: u64,
        entries: Vec<ContextEntryView>,
    },
    ContextEntriesRemoved {
        base_revision: u64,
        revision: u64,
        entry_ids: Vec<ItemId>,
        reason: String,
    },
    ContextKeysRemoved {
        base_revision: u64,
        revision: u64,
        keys: Vec<String>,
    },
    ContextKeyPrefixReplaced {
        base_revision: u64,
        revision: u64,
        key_prefix: String,
        entries: Vec<ContextEntryView>,
    },
    ContextStateReplaced {
        base_revision: u64,
        revision: u64,
        entries: Vec<ContextEntryView>,
        reason: String,
    },
    ContextCompactionRequested {
        base_revision: u64,
        revision: u64,
        trigger: String,
    },
    ContextCompactionFinished {
        base_revision: u64,
        revision: u64,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_ref: Option<String>,
    },
    SkillCatalogSet {
        catalog_ref: Option<String>,
    },
    SkillActivationsSet {
        skill_ids: Vec<String>,
    },
    ToolsReplaced {
        base_revision: u64,
        revision: u64,
    },
    ToolsPatched {
        base_revision: u64,
        revision: u64,
        upserted: Vec<String>,
        removed: Vec<String>,
    },
    ToolDefaultTargetChanged {
        namespace: String,
        target: Option<ToolExecutionTargetView>,
    },
    ToolBatchStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        calls: Vec<ToolCallEventView>,
    },
    ToolCallStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
    },
    ToolCallCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
        status: ToolItemStatus,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        effects: Vec<ToolEffectView>,
    },
    ToolBatchDeferred {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
    ToolBatchResumed {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
    ToolBatchCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunAcceptedSourceView {
    Input { entries: Vec<ContextEntryInputView> },
    Context { keys: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionTargetView {
    pub namespace: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEventView {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}
