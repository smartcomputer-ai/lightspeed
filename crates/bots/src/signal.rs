//! The controller workflow's wire: its start/config payload, the event
//! signal, the rotate signal, the delivery receipt, and the signal and
//! query names. The event row in Postgres is authoritative; the signal is
//! a notification carrying only what routing and delivery need.

use api::{BotCoalescePolicy, BotEventMedia, BotEventOutcome, BotId, ProfileId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::records::{BotRecord, RoutedSession};

pub const BOT_EVENT_SIGNAL: &str = "bot_event";
pub const BOT_CONFIG_SIGNAL: &str = "bot_config";
pub const BOT_SESSION_ROTATE_SIGNAL: &str = "bot_session_rotate";
pub const BOT_STATE_QUERY: &str = "bot_state";
/// Delivery receipts to an admitting source's workflow (`BotEvent::notify`):
/// `started` when the run begins, `finished` when the delivery ends.
pub const BOT_DELIVERY_SIGNAL: &str = "bot_delivery";

/// Durable controller configuration; one per bot record revision. Sent as
/// the workflow's start argument and again with every `bot_config` signal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotControllerConfig {
    pub universe_id: Uuid,
    pub bot_id: BotId,
    pub display_name: Option<String>,
    pub profile_id: ProfileId,
    pub brief: Option<String>,
    pub runs_per_day: Option<u32>,
    pub routed_session_close_after_ms: Option<u64>,
    #[serde(default)]
    pub self_config: bool,
    #[serde(default)]
    pub emit: bool,
    #[serde(default)]
    pub enabled: bool,
    /// Terminal teardown: archive what is pending, force-close every
    /// session, record them on the row, complete instead of continuing as
    /// new. Idempotent.
    #[serde(default)]
    pub closed: bool,
}

impl BotControllerConfig {
    pub fn from_record(universe_id: Uuid, record: &BotRecord) -> Self {
        Self {
            universe_id,
            bot_id: record.bot_id.clone(),
            display_name: record.document.display_name.clone(),
            profile_id: record.document.profile_id.clone(),
            brief: record.document.brief.clone(),
            runs_per_day: record.document.runs_per_day,
            routed_session_close_after_ms: record.document.routed_session_close_after_ms,
            self_config: record.document.self_config,
            emit: record.document.emit,
            enabled: record.document.enabled,
            closed: record.is_closed(),
        }
    }
}

/// Coalescing directives computed at admission from the trigger. Events
/// sharing a key accumulate in one controller buffer and flush as one
/// delivery carrying the whole batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCoalesceParams {
    pub key: String,
    pub debounce_ms: u64,
    pub max_wait_ms: u64,
    pub max_count: u32,
}

impl BotCoalesceParams {
    pub fn from_policy(key: String, policy: BotCoalescePolicy) -> Self {
        Self {
            key,
            debounce_ms: policy.debounce_ms,
            max_wait_ms: policy.max_wait_ms,
            max_count: policy.max_count,
        }
    }
}

/// The `bot_event` signal body: minimal and deterministic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotEvent {
    pub id: String,
    /// Per-bot sequence number: the only handle models and humans use.
    pub seq: u64,
    /// CAS ref of the envelope document.
    pub document_ref: String,
    /// CAS ref of the rendering delivered to sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<RoutedSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce: Option<BotCoalesceParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_busy: Option<api::BotWhenBusy>,
    /// Federation hop count carried into the delivery so emits from it can
    /// be bounded.
    #[serde(default)]
    pub hops: u32,
    /// The sender asked for a receipt when this delivery finishes.
    #[serde(default)]
    pub reply: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<BotEventMedia>,
    /// CAS ref of receiver-bound tool declarations a routed session is
    /// created with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_ref: Option<String>,
    /// The admitting source asked for `started` / `finished` receipts.
    #[serde(default)]
    pub notify: bool,
}

/// Operator request to close one controlled session and advance its
/// generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotSessionRotate {
    pub session_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotDeliveryPhase {
    Started,
    Finished,
}

/// The `bot_delivery` signal body a notified workflow receives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotDeliveryReceipt {
    /// The admitting source's opaque token, echoed verbatim.
    pub token: String,
    pub phase: BotDeliveryPhase,
    pub delivery_id: String,
    /// `#N`s of the delivery's events.
    pub seqs: Vec<u64>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// `finished` only: the lane's outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotEventOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}
