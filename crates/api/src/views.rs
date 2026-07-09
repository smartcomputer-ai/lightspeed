use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub status: SessionStatus,
    pub config_revision: u64,
    /// The stored sparse config document, exactly as last put (model and
    /// feature versions materialized at admission). Effective tool reality
    /// is visible via `active_tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfig>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub runs: Vec<RunView>,
    pub active_context: ContextView,
    #[serde(default)]
    pub active_tools: ActiveToolsView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vfs_mounts: Vec<VfsMountView>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextView {
    pub revision: u64,
    #[serde(default)]
    pub entries: Vec<ContextEntryView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    NotLoaded,
    Idle,
    Active,
    Closed,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: RunId,
    pub status: RunStatus,
    pub source: RunViewSource,
    #[serde(default)]
    pub entries: Vec<ContextEntryView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_batches: Vec<ToolBatchView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunViewSource {
    Input { items: Vec<InputItem> },
    Context { keys: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatchView {
    pub id: String,
    pub turn_id: String,
    pub status: ToolItemStatus,
    #[serde(default)]
    pub calls: Vec<ToolCallView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallView {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    pub status: ToolItemStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffectView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolEffectView {
    pub kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDisplayView {
    pub group: ToolCallDisplayGroup,
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallDisplayGroup {
    Explore,
    Edit,
    Execute,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderContextDisplayView {
    pub summary: ToolCallDisplayView,
    pub tool_name: String,
    pub status: ToolItemStatus,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InputItem {
    Text {
        text: String,
    },
    TextRef {
        blob_ref: String,
    },
    Media {
        blob_ref: String,
        mime: String,
        kind: MediaKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MediaKind {
    Image,
    Audio,
    Document,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntryInputView {
    pub kind: ContextEntryKindView,
    pub content_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<TokenEstimateView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContextEntryKindView {
    Message { role: ContextMessageRoleView },
    Instructions,
    VfsCatalog,
    EnvironmentCatalog,
    EnvironmentActive,
    SkillCatalog,
    SkillActivation { skill_id: SkillId },
    ToolCall { call_id: String, name: String },
    ToolResult { call_id: String, is_error: bool },
    ReasoningState,
    ProviderOpaque,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextMessageRoleView {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenEstimateView {
    pub tokens: u32,
    pub quality: TokenEstimateQualityView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TokenEstimateQualityView {
    Exact,
    ProviderCounted,
    Estimated,
}

/// A session context entry, faithful to the stored engine entry: keyed,
/// kind-tagged, ref-backed. Keys are a stable extension point — clients
/// reconstruct derived surfaces (e.g. the prompted instruction set via the
/// `prompt_instructions/` key prefix) by filtering on `key` and fetching
/// bodies through `blobs/read`. `text` inlines blob content only for
/// message and tool entries; every other kind is ref + preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntryView {
    pub id: ItemId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub kind: ContextEntryKindView,
    pub content_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<TokenEstimateView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ProviderContextDisplayView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolItemStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Unavailable,
}
