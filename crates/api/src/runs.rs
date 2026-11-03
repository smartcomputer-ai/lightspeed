use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStartParams {
    pub session_id: SessionId,
    pub source: RunStartSource,
    /// Client-supplied idempotency key, unique per session. Retrying
    /// `session/runs/start` with the same submission id and the same
    /// source/config/terminal notification returns the original run instead
    /// of starting a second one; reusing a submission id with any of those
    /// inputs changed is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<RunStartConfig>,
    /// Request an at-least-once terminal emission to the managed session's
    /// immutable lifecycle controller. The destination is derived by the
    /// gateway; callers supply only the controller's opaque dedup token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_on_terminal: Option<RunTerminalNotificationInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunListParams {
    pub session_id: SessionId,
    /// Exclusive upper run-id bound from a previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunListResponse {
    #[serde(default)]
    pub runs: Vec<RunSummaryView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<RunId>,
    pub has_older_runs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunReadParams {
    pub session_id: SessionId,
    pub run_id: RunId,
}

/// The complete projection of one run. Run projection is stateful across the
/// run's events, so partial pages would silently lose cross-event state; a
/// run whose interval exceeds the server's detail ceiling is rejected with a
/// typed error and remains readable through `session/events/read`. Inline
/// message and reasoning text is complete. Tool and catalog previews are bounded;
/// their full bodies remain available through `blobs/read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunReadResponse {
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunTerminalNotificationInput {
    pub token: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunStartSource {
    Input { items: Vec<InputItem> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStartConfig {
    /// Optional model override within the session's provider identity and API
    /// kind. Changing either requires a new session, even before the first run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<RunLimitsConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunLimitsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResponse {
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunCancelParams {
    pub session_id: SessionId,
    pub run_id: RunId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunCancelResponse {
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunApprovalsDecideParams {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub decisions: Vec<ApprovalDecisionInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionInput {
    pub approval_id: String,
    pub decision: ApprovalDecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecisionKind {
    Approve,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunApprovalsDecideResponse {
    pub results: Vec<ApprovalDecisionResult>,
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionResult {
    pub approval_id: String,
    pub status: ApprovalDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ApprovalDecisionFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecisionStatus {
    Decided,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionFailure {
    pub kind: ApprovalDecisionFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecisionFailureKind {
    InvalidId,
    Unknown,
    ForeignRun,
    AlreadyDecided,
    Cancelled,
    Duplicate,
    InvalidNote,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSteerParams {
    pub session_id: SessionId,
    /// The run to steer. Must be the session's current active run (running
    /// or parked on an await); a finished or cancelling run is rejected so a
    /// late steer never lands on the next run.
    pub run_id: RunId,
    /// Steering input, same vocabulary as run input. Delivered to the model
    /// at the next turn boundary of the run; it does not interrupt the
    /// in-flight turn or wake a parked await. If the run fails before that
    /// boundary, the input still enters context and the next run sees it as
    /// history. Cancelling the run drops it.
    pub items: Vec<InputItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSteerResponse {
    /// Identifier of the accepted steering batch within the session.
    pub steering_id: String,
    pub run: RunView,
}
