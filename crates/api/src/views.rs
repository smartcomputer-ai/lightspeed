use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Descriptive key/value metadata; empty when none was set.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_ms: Option<u64>,
    pub retention: SessionRetentionView,
    /// True only when immutable lifecycle ownership was admitted with a
    /// lifecycle controller at managed-session creation.
    pub managed: bool,
    pub config_revision: u64,
    /// The stored sparse config document, exactly as last put (model and
    /// feature versions materialized at admission). Effective tool reality
    /// is visible via `active_tools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfig>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub runs: Vec<RunSummaryView>,
    /// The currently executing run, always present when one exists — the
    /// paged `runs` window can omit it behind newer queued runs, so control
    /// surfaces (steering, status) must read it from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run: Option<RunSummaryView>,
    pub active_context: ContextView,
    #[serde(default)]
    pub active_tools: ActiveToolsView,
    /// The universe environment selected by the session event log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<EnvironmentId>,
    /// Immutable workflow-backed tool declaration. A lifecycle controller
    /// indicates external session ownership; tool-only declarations do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management: Option<SessionManagementView>,
    /// Sub-agent lineage; absent for root sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOriginView>,
}

/// Bounded run metadata projected from reducer state. Transcript entries and
/// tool payloads are available only from `session/runs/read` or the event
/// stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSummaryView {
    pub id: RunId,
    pub status: RunStatus,
    pub accepted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    pub source: RunSummarySourceView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsageView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_approvals: Vec<PendingApprovalView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunSummarySourceView {
    Input {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        preview_truncated: bool,
    },
}

/// Managed-session reads use the same immutable declaration document accepted
/// at creation. Per-invocation diagnostics remain in `session/events/read`.
pub type SessionManagementView = ManagedSessionWorkflowToolsInput;

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
    /// Authoritative terminal output, retained independently of active context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<crate::ContentRefView>,
    /// Complete visible text derived from `output`; absent for non-text output
    /// or runs without a terminal output. Native metadata stays in the blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_text: Option<String>,
    /// When the run left the queue and began executing, derived from the
    /// committed `runStarted` event. Absent while the run is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// When the run reached a terminal state, derived from its committed
    /// completed, failed, or cancelled event. Absent for non-terminal runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    pub source: RunViewSource,
    #[serde(default)]
    pub entries: Vec<ContextEntryView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_batches: Vec<ToolBatchView>,
    /// Provider token usage summed over the run's completed generations;
    /// absent until the first generation reports usage. The cached share
    /// (`cachedInputTokens / inputTokens`) is the prompt-cache hit rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsageView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_approvals: Vec<PendingApprovalView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingApprovalView {
    pub approval_id: String,
    pub requested_at_ms: u64,
    pub subject: ApprovalSubjectView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApprovalSubjectView {
    McpToolCall {
        server_id: String,
        server_label: String,
        tool_name: String,
        arguments_ref: String,
        arguments_preview: String,
    },
}

/// Provider-reported token usage for one generation or a sum of them. Every
/// field is optional because providers report different subsets; counts
/// that a provider reports separately (Anthropic's cache read/write) are
/// folded into `input_tokens` so the field always means "prompt tokens
/// billed on this request, cached or not".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsageView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    /// Prompt tokens served from the provider's prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
    /// Prompt tokens written into the provider's prompt cache (Anthropic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunViewSource {
    Input { items: Vec<InputItem> },
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
    /// Admitted registry identity, absent when the model used an unavailable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
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
    /// When the call was dispatched for execution, from the committed
    /// `toolCallStarted` event. Absent until dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// When the call's terminal result was committed. The window to
    /// `startedAtMs` includes runtime scheduling overhead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    /// Execution milliseconds measured by the runtime around the call body;
    /// the remainder of the started/completed window is overhead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
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

/// Coarse activity family of a tool call, chosen by the projection from the
/// tool name. Clients key icons and colours on it; the verb and target carry
/// the specifics. Unknown tools land in `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallDisplayGroup {
    /// Reads the environment or the web without changing it.
    Explore,
    /// Writes or patches files.
    Edit,
    /// Runs or controls a process.
    Execute,
    /// Calls a tool on an MCP server, through the builtin bridge or a
    /// provider-native MCP block.
    Mcp,
    /// Delegates to a sub-agent or waits on one.
    Agent,
    /// Bot-to-bot traffic and a bot's own configuration tools.
    Bot,
    /// Sends into a chat conversation on a channel.
    Message,
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

/// A source the assistant cited, in the same shape for every provider. The
/// exact provider output stays in a separate opaque context entry for replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CitationView {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Queued,
    Running,
    Parked,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Application-supplied display provenance (1–200 nonblank bytes).
        /// The platform uses `user:<id>` for direct human input and `event` for
        /// bot deliveries; other values are allowed. Omitted means unknown.
        /// This metadata is not an authorization identity or model input text.
        origin: Option<String>,
        text: String,
    },
    TextRef {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Application-supplied display provenance (1–200 nonblank bytes).
        /// The platform uses `user:<id>` for direct human input and `event` for
        /// bot deliveries; other values are allowed. Omitted means unknown.
        /// This metadata is not an authorization identity or model input text.
        origin: Option<String>,
        blob_ref: String,
    },
    Media {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        /// Application-supplied display provenance (1–200 nonblank bytes).
        /// The platform uses `user:<id>` for direct human input and `event` for
        /// bot deliveries; other values are allowed. Omitted means unknown.
        /// This metadata is not an authorization identity or model input text.
        origin: Option<String>,
        blob_ref: String,
        mime: String,
        kind: MediaKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A client-owned catalog document: what the model may pick from (a
    /// directory, a roster, a menu), rendered by the client as text. Accepted
    /// only by `session/context/append` under a client key; run input rejects
    /// it. A changed catalog supersedes the earlier version instead of
    /// replacing it, so the earlier version stays rendered and the provider
    /// prefix cache holds; superseded versions are dropped at the next
    /// context rewrite or beyond a per-key cap.
    Catalog {
        /// Short name shown as the catalog's heading, e.g. "Bot directory".
        title: String,
        /// The catalog body, plain text or Markdown.
        text: String,
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
    /// Application-supplied display origin, independent of role and insertion source.
    /// `user:<id>` identifies platform composer input; `event` marks bot deliveries.
    /// Other values are allowed; omission means unknown. Not an authorization identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub kind: ContextEntryKindView,
    pub content: crate::ContentRefView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Immutable artifact recording this entry's origin or construction.
    pub provenance_ref: Option<String>,
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
    Message {
        role: ContextMessageRoleView,
    },
    Instructions,
    /// Runtime or client catalog with a stored text body and independent key.
    Catalog {
        title: String,
    },
    ToolCall {
        call_id: String,
        name: String,
    },
    ToolResult {
        call_id: String,
        is_error: bool,
    },
    ReasoningState,
    ProviderOpaque,
    McpApprovalResponse {
        approve: bool,
    },
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
/// original bytes through `blobs/read`. `text` contains complete message and
/// visible reasoning text; tool and catalog bodies use bounded text previews.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntryView {
    /// Application-supplied display origin, independent of role and insertion source.
    /// `user:<id>` identifies platform composer input; `event` marks bot deliveries.
    /// Other values are allowed; omission means unknown. Not an authorization identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub id: ItemId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub kind: ContextEntryKindView,
    pub content: crate::ContentRefView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Immutable artifact recording this entry's origin or construction.
    pub provenance_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Native item identity derived from the payload, when present.
    pub provider_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<TokenEstimateView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// True when a tool or catalog body's `text` is a bounded prefix. Fetch
    /// its original content through `blobs/read`. Messages and reasoning are full.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub text_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ProviderContextDisplayView>,
    /// Sources this assistant message cites, derived from its own native
    /// payload. Empty for every other entry kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationView>,
    /// Where the entry came from: run input, a steering batch, model output,
    /// a tool result, reasoning state, a context edit, or the runtime. Lets
    /// transcripts render steering distinctly from the run's first input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ContextEntrySourceView>,
    /// For a catalog entry: the earlier version of the same keyed catalog
    /// that this entry updates. The earlier entry stays in context until a
    /// rewrite drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<ItemId>,
    /// For a catalog entry that a newer version has updated: that newer
    /// entry. Present only on state views (`session/read`), where the whole
    /// active context is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<ItemId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContextEntrySourceView {
    ContextEdit,
    RunInput {
        run_id: RunId,
        input_index: u32,
    },
    Steering {
        run_id: RunId,
        steering_id: String,
        input_index: u32,
    },
    AssistantOutput {
        run_id: RunId,
        turn_id: String,
    },
    ApprovalDecision {
        run_id: RunId,
        approval_id: String,
    },
    Tool {
        run_id: RunId,
        turn_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch_id: Option<String>,
    },
    Reasoning {
        run_id: RunId,
        turn_id: String,
    },
    Runtime {
        label: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolItemStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unavailable,
}
