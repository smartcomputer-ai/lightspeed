use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    pub session_id: Option<SessionId>,
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfigInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileSource>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_defaults: Option<RunDefaultsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolConfigInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoiceConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolChoiceConfig {
    pub mode: ToolChoiceModeConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_parallel_tool_use: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolChoiceModeConfig {
    Auto,
    None,
    RequiredAny,
    Specific { tool_id: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionPolicyInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum CompactionPolicyInput {
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDefaultsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemToolMode>,
    /// Enables the messaging toolset (message_send/react/edit/noop) for
    /// sessions bound to a chat channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<bool>,
    /// Enables the Fleet subagent control-plane tools
    /// (agent_spawn/send/read/list/cancel and profile_list/read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FilesystemToolMode {
    None,
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_config_revision: Option<u64>,
    #[serde(default)]
    pub patch: SessionConfigPatchInput,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfigPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigPatchInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_defaults: Option<RunDefaultsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolConfigPatchInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "op", content = "value")]
#[schemars(rename = "FieldPatchOf{T}")]
pub enum FieldPatch<T> {
    Set(T),
    Clear,
}

impl<T> FieldPatch<T> {
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Clear => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<FieldPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<FieldPatch<ToolChoiceConfig>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextConfigPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<FieldPatch<CompactionPolicyInput>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDefaultsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<FieldPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<FieldPatch<u32>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<FieldPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<FieldPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FieldPatch<FilesystemToolMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<FieldPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<FieldPatch<bool>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolsUpdateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_tools_revision: Option<u64>,
    pub update: SessionToolsUpdateInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionToolsUpdateInput {
    Replace {
        #[serde(default)]
        tools: Vec<ToolView>,
    },
    Patch {
        #[serde(default)]
        upsert: Vec<ToolView>,
        #[serde(default)]
        remove: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolsUpdateResponse {
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
    pub applied_keys: Vec<String>,
    pub unchanged_keys: Vec<String>,
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
    SessionClosed,
    RunAccepted {
        run_id: RunId,
        submission_id: Option<String>,
        input: Vec<ContextEntryInputView>,
    },
    RunStarted {
        run_id: RunId,
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
        items: Vec<SessionItemView>,
    },
    ContextEntriesRemoved {
        base_revision: u64,
        revision: u64,
        item_ids: Vec<ItemId>,
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
        items: Vec<SessionItemView>,
    },
    ContextStateReplaced {
        base_revision: u64,
        revision: u64,
        items: Vec<SessionItemView>,
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
