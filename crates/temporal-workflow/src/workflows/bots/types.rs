//! Activity request/result shapes and workflow arguments of the bot
//! workflows. Everything here crosses a Temporal boundary and is therefore
//! serde-stable; the domain vocabulary comes from the `bots` crate.

use api::{BotEventOutcome, BotId, BotTriggerId, LlmUsageView, ProfileId};
use bots::{BotDeliveryPhase, BotEvent};
use harness::WorkflowEndpointRef;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Sessions ────────────────────────────────────────────────────────────────

/// Create or reconcile one of the bot's managed sessions: the profile and
/// brief applied, the `bot_*` tools (and any carried receiver-bound tools)
/// declared, the controller recorded as lifecycle controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotEnsureSessionRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub session_id: String,
    /// Original key for a keyed thread; never a derived session id or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Human label of the routed thread, without the bot's display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub profile_id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(default)]
    pub self_config: bool,
    #[serde(default)]
    pub emit: bool,
    /// The profile revision this session already runs; the activity applies
    /// the profile again only when the catalog moved past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_profile_revision: Option<u64>,
    /// The controller itself: receiver of every pushed `bot_*` invocation
    /// and the session's lifecycle controller.
    pub controller: WorkflowEndpointRef,
    /// CAS ref of receiver-bound declarations to merge after the bot tools
    /// (a chat conversation's `message_*` tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotEnsureSessionResult {
    Ready {
        profile_revision: u64,
        #[serde(default)]
        carried_tool_ids: Vec<String>,
    },
    /// The session exists under another tool declaration; declarations are
    /// immutable per session, so the controller rotates to a successor.
    DeclarationMismatch { message: String },
    /// The session cannot take the profile's new config (its provider api
    /// kind is pinned for its lifetime); the controller rotates.
    ProfileUnapplicable { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSessionRequest {
    pub universe_id: Uuid,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRenameSessionRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotSessionStatus {
    Idle,
    Busy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    Closed,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotReadRunUsageRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    pub run_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotReadRunUsageResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsageView>,
}

// ── Deliveries ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotStartRunRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    pub delivery_id: String,
    pub events: Vec<BotEvent>,
    /// Deterministic per delivery so retries converge on one run.
    pub submission_id: String,
    /// Run-terminal notify token addressed to the controller.
    pub terminal_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotStartRunResult {
    Started {
        run_id: String,
    },
    /// The session refused the run (not open, busy race); the lane retries
    /// later.
    Rejected {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSteerRunRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    pub delivery_id: String,
    pub events: Vec<BotEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotSteerRunResult {
    Steered {
        run_id: String,
    },
    /// No run was active (or the steer raced a terminal); the lane falls
    /// back to queueing.
    NotRunning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAppendContextRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    pub delivery_id: String,
    pub events: Vec<BotEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCloseSessionRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    /// Force-close even while busy (teardown); otherwise a busy session is
    /// left alone and `closed` is false.
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCloseSessionResult {
    pub closed: bool,
    #[serde(default)]
    pub descendants_closed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCountDescendantsRequest {
    pub universe_id: Uuid,
    /// Root sessions whose delegation trees count (the bot's sessions that
    /// started runs today).
    pub session_ids: Vec<String>,
    pub since_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCountDescendantsResult {
    pub count: u32,
}

// ── Pushed tools and resolves ───────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotReadToolInvocationsRequest {
    pub universe_id: Uuid,
    pub session_id: String,
    /// Session log cursor; only events after it are read.
    pub after_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotToolInvocationRef {
    pub invocation_id: String,
    pub tool_id: String,
    pub run_id: String,
    pub arguments_ref: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotReadToolInvocationsResult {
    pub next_seq: u64,
    #[serde(default)]
    pub invocations: Vec<BotToolInvocationRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotReadJsonBlobRequest {
    pub universe_id: Uuid,
    pub blob_ref: String,
}

/// What the tool executor may tell the model about the controller: labels,
/// `#N`s, counts — never ids the model must copy back. Plus the private
/// invocation context `bot_emit` needs (hops, the routed session).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotControllerSummary {
    pub snapshot: api::BotControllerSnapshot,
    /// Federation hop count of the delivery whose run invoked the tool.
    #[serde(default)]
    pub hops: u32,
    /// The routed session (base id and label) the invoking run belongs to,
    /// when not the main session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_session: Option<bots::RoutedSession>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotExecuteToolRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub session_id: String,
    pub invocation_id: String,
    pub tool_id: String,
    pub arguments_ref: String,
    pub controller: BotControllerSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotExecuteToolResult {
    Resolved {
        payload_ref: String,
    },
    /// A typed refusal or validation failure the model should read; the
    /// error document is in the CAS.
    Failed {
        message: String,
        error_ref: String,
    },
}

// ── Outcomes, receipts, directory ───────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRecordOutcomesRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub event_ids: Vec<String>,
    pub outcome: BotEventOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRecordOutcomesResult {
    pub updated: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRecordClosedRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub sessions: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRecordClosedResult {
    pub sessions: Vec<String>,
}

/// Signal `bot_delivery` receipts to every notify endpoint recorded on the
/// listed events, one per distinct `(workflow, token)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSendDeliveryReceiptsRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub event_ids: Vec<String>,
    pub phase: BotDeliveryPhase,
    pub delivery_id: String,
    pub seqs: Vec<u64>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotEventOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotReceiptsSent {
    pub sent: u32,
    #[serde(default)]
    pub skipped: u32,
}

/// Admit a `bot.reply` receipt into the inbox of every asking bot among the
/// listed events (those with a reply route).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSendBotReceiptsRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub delivery_id: String,
    pub event_ids: Vec<String>,
    pub outcome: BotEventOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Highest hop count of the delivery; receipts hop once more.
    pub hops: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPublishDirectoryRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub session_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPublishDirectoryResult {
    pub entries: u32,
}

// ── Trigger fires ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotTriggerFireKind {
    Schedule,
    Poll,
}

/// Start argument of `BotTriggerFireWorkflow`; the Schedule action carries
/// it and the trigger row is re-read at fire time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotTriggerFireArgs {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub kind: BotTriggerFireKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotTriggerFireRequest {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    /// The Schedule's nominal fire time (`TemporalScheduledStartTime`), or
    /// the workflow's start time when started by hand.
    pub scheduled_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotScheduleFireResult {
    Admitted {
        event_id: String,
        duplicate: bool,
    },
    /// The trigger or bot is gone, disabled, or the breaker tripped.
    Refused {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotPollFireResult {
    Polled {
        baselined: bool,
        admitted: u32,
        filtered: u32,
    },
    Refused {
        reason: String,
    },
}
