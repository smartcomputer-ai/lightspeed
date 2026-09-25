//! Bot, trigger, and event records with their store contracts. Records are
//! the wire views plus the columns the runtime keeps to itself (secrets,
//! receipt routes, the event counter); the stores are universe-scoped and
//! implemented by `store-pg` and [`crate::memory`].

use api::{
    BotDocument, BotEventMedia, BotEventOutcome, BotEventReplyRef, BotEventView, BotId,
    BotRoutedSessionView, BotTriggerDisabledReason, BotTriggerDocument, BotTriggerId,
    BotTriggerKind, BotTriggerView, BotView, PollCursorState, ProfileId,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::BotError;

// ── Bots ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRecord {
    pub bot_id: BotId,
    pub revision: u64,
    pub document: BotDocument,
    /// Monotonic per-bot event counter; allocated at admission, shown as
    /// `#N`.
    pub event_seq: u64,
    pub closed_at_ms: Option<i64>,
    /// Sessions the controller closed on the way out, recorded by the
    /// controller itself so `bots/delete` can erase them.
    pub closed_sessions: Vec<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl BotRecord {
    pub fn is_closed(&self) -> bool {
        self.closed_at_ms.is_some()
    }

    pub fn view(&self) -> BotView {
        BotView {
            bot_id: self.bot_id.clone(),
            revision: self.revision,
            document: self.document.clone(),
            event_seq: self.event_seq,
            closed_at_ms: self.closed_at_ms,
            closed_sessions: self.closed_sessions.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

/// Roster row: the bot plus what the console needs at a glance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotRosterRow {
    pub bot: BotRecord,
    pub trigger_count: u32,
    pub pending_count: u64,
    pub last_event: Option<BotEventRecord>,
}

#[async_trait]
pub trait BotStore: Send + Sync {
    async fn create_bot(
        &self,
        bot_id: BotId,
        document: BotDocument,
        now_ms: i64,
    ) -> Result<BotRecord, BotError>;

    /// Replace the whole document and bump the revision. `expected_revision`
    /// is checked when given; a closed bot rejects everything but a change
    /// of `display_name` / `description`.
    async fn put_bot(
        &self,
        bot_id: BotId,
        document: BotDocument,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<BotRecord, BotError>;

    async fn read_bot(&self, bot_id: &BotId) -> Result<BotRecord, BotError>;

    async fn list_bots(&self) -> Result<Vec<BotRecord>, BotError>;

    /// Bots plus their trigger count, pending event count, and latest
    /// event, ordered by bot id.
    async fn list_bot_roster(&self) -> Result<Vec<BotRosterRow>, BotError>;

    /// Open (not closed) bots whose profile is `profile_id`.
    async fn list_bots_for_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<Vec<BotRecord>, BotError>;

    /// Mark the bot closed: `closed_at_ms` set once, `enabled` cleared.
    /// Idempotent — a closed bot returns its record unchanged.
    async fn close_bot(&self, bot_id: &BotId, now_ms: i64) -> Result<BotRecord, BotError>;

    /// Union `sessions` into the bot's recorded closed sessions.
    async fn record_bot_closed_sessions(
        &self,
        bot_id: &BotId,
        sessions: Vec<String>,
    ) -> Result<Vec<String>, BotError>;

    async fn delete_bot(&self, bot_id: &BotId) -> Result<BotRecord, BotError>;

    /// Race-free `#N` allocation: increments and returns the bot's counter.
    async fn allocate_bot_event_seq(&self, bot_id: &BotId) -> Result<u64, BotError>;
}

// ── Triggers ────────────────────────────────────────────────────────────────

/// Server-minted trigger secrets, kept beside the document and shown only
/// to managing principals.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotTriggerSecrets {
    /// Webhook triggers: the URL path token (24 random bytes, hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_token: Option<String>,
    /// Chat triggers with `pairing: code`: the pairing code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotTriggerRecord {
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub revision: u64,
    pub document: BotTriggerDocument,
    pub secrets: BotTriggerSecrets,
    pub disabled_reason: Option<BotTriggerDisabledReason>,
    pub disabled_at_ms: Option<i64>,
    pub last_filter_error: Option<String>,
    pub last_filter_error_at_ms: Option<i64>,
    pub cursor: Option<PollCursorState>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl BotTriggerRecord {
    pub fn kind(&self) -> BotTriggerKind {
        self.document.spec.kind()
    }

    pub fn enabled(&self) -> bool {
        self.document.enabled
    }

    /// Whether this trigger is driven by a Temporal Schedule.
    pub fn has_schedule(&self) -> bool {
        matches!(self.kind(), BotTriggerKind::Schedule | BotTriggerKind::Poll)
    }

    /// The wire view. `redact` hides the ingest path and pairing code
    /// (channel-facing views); `ingest_path` is the webhook route when
    /// the caller can build one.
    pub fn view(&self, redact: bool, ingest_path: Option<String>) -> BotTriggerView {
        BotTriggerView {
            bot_id: self.bot_id.clone(),
            trigger_id: self.trigger_id.clone(),
            revision: self.revision,
            document: self.document.clone(),
            disabled_reason: self.disabled_reason,
            disabled_at_ms: self.disabled_at_ms,
            last_filter_error: self.last_filter_error.clone(),
            last_filter_error_at_ms: self.last_filter_error_at_ms,
            cursor_state: self.cursor.clone(),
            ingest_path: if redact { None } else { ingest_path },
            pairing_code: if redact {
                None
            } else {
                self.secrets.pairing_code.clone()
            },
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

/// What a put writes: the document and the secrets that go with it. The
/// caller (the service) carries existing secrets forward across edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotTriggerWrite {
    pub trigger_id: BotTriggerId,
    pub document: BotTriggerDocument,
    pub secrets: BotTriggerSecrets,
    /// A spec edit of a poll trigger resets the cursor; `Some(None)` clears
    /// it, `None` leaves it as stored.
    pub cursor: Option<Option<PollCursorState>>,
}

#[async_trait]
pub trait BotTriggerStore: Send + Sync {
    /// Create when absent, otherwise replace the document and secrets and
    /// bump the revision. A replaced trigger keeps its incidents unless the
    /// document enables it again, which clears `disabled_reason`.
    async fn put_bot_trigger(
        &self,
        bot_id: &BotId,
        write: BotTriggerWrite,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<BotTriggerRecord, BotError>;

    async fn read_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, BotError>;

    /// The bot's triggers ordered by trigger id.
    async fn list_bot_triggers(&self, bot_id: &BotId) -> Result<Vec<BotTriggerRecord>, BotError>;

    /// Every trigger of the universe with the given kind, ordered by bot id
    /// then trigger id (schedule reconciliation, chat trigger candidates).
    async fn list_bot_triggers_by_kind(
        &self,
        kind: BotTriggerKind,
    ) -> Result<Vec<BotTriggerRecord>, BotError>;

    async fn delete_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, BotError>;

    /// Runtime disable (breaker, poll failures, one-shot, bot close): sets
    /// `enabled = false` with the reason. Re-enabling is a document put.
    async fn disable_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        reason: BotTriggerDisabledReason,
        now_ms: i64,
    ) -> Result<BotTriggerRecord, BotError>;

    /// Disable every enabled trigger of the bot; returns the ones changed.
    async fn disable_bot_triggers(
        &self,
        bot_id: &BotId,
        reason: BotTriggerDisabledReason,
        now_ms: i64,
    ) -> Result<Vec<BotTriggerRecord>, BotError>;

    /// Record (or clear, with `None`) the last runtime filter failure.
    async fn set_bot_trigger_filter_error(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        error: Option<String>,
        now_ms: i64,
    ) -> Result<(), BotError>;

    async fn set_bot_trigger_cursor(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        cursor: Option<PollCursorState>,
    ) -> Result<(), BotError>;
}

// ── Events ──────────────────────────────────────────────────────────────────

/// Retention of a routed session, decided at admission from the trigger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RoutedSessionClosePolicy {
    /// Inherit the bot's `routedSessionCloseAfterMs`.
    #[default]
    Inherit,
    /// Never close (chat).
    Never,
    After {
        ms: u64,
    },
}

/// Routing target computed at admission; absent means the main session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutedSession {
    pub session_id: String,
    pub label: String,
    /// Original per-key routing value, distinct from the display label and
    /// derived session id. Absent for per-event routes and older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(default)]
    pub close_policy: RoutedSessionClosePolicy,
}

impl RoutedSession {
    pub fn view(&self) -> BotRoutedSessionView {
        BotRoutedSessionView {
            session_id: self.session_id.clone(),
            label: self.label.clone(),
        }
    }
}

/// Who admitted the event and hears back when its delivery finishes.
/// Private to the runtime: never part of a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EventReceiver {
    /// The admitting workflow (a chat conversation): signalled `started` /
    /// `finished` delivery receipts with its token, and the owner of the
    /// receiver-bound tool declarations (`message_*`) at `tools_ref` that
    /// the routed session is created with.
    Workflow {
        workflow_id: String,
        workflow_kind: String,
        token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_ref: Option<String>,
    },
    /// The asking bot of an addressed `bot_emit { reply: true }`: sent a
    /// `bot.reply` receipt at its logical session (base id, never a
    /// generation).
    Bot {
        bot_id: BotId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<RoutedSession>,
    },
}

impl EventReceiver {
    /// CAS ref of the receiver-bound tool declarations, when the receiver
    /// serves any.
    pub fn tools_ref(&self) -> Option<&str> {
        match self {
            Self::Workflow { tools_ref, .. } => tools_ref.as_deref(),
            Self::Bot { .. } => None,
        }
    }
}

/// One row of the bot's numbered event log. The groups mirror the
/// `bot_events` table: identity, what arrived, the delivery plan computed
/// at admission, federation, the receiver, and the write-once outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotEventRecord {
    // ── Identity and order ──
    pub bot_id: BotId,
    /// Dedupe identity: the provider delivery id where known, otherwise
    /// derived at admission.
    pub event_id: String,
    /// Per-bot `#N`; the only event handle shown to models and humans.
    pub seq: u64,

    // ── What arrived ──
    /// Admitting trigger; `None` for an operator admit. History outlives
    /// the trigger.
    pub trigger_id: Option<BotTriggerId>,
    /// Event kind as authored by the source (`github.push`,
    /// `schedule.fire`, `chat.message`, `bot.reply`).
    pub kind: String,
    pub summary: String,
    /// When the source says it happened.
    pub occurred_at_ms: i64,
    /// Admission time; log order and rate windows are keyed on it.
    pub received_at_ms: i64,
    /// CAS ref of the envelope document.
    pub document_ref: String,

    // ── Delivery plan, computed at admission ──
    /// CAS ref of the model-facing rendering delivered to sessions; pins
    /// what the session saw. `None` only for archived rows.
    pub prompt_ref: Option<String>,
    /// The routed session; `None` means the bot's main session.
    pub session: Option<RoutedSession>,
    /// Prepared attachments appended to the run input.
    pub media: Vec<BotEventMedia>,

    // ── Federation ──
    /// Sending bot for bot-originated events; counted against the sender
    /// rate cap.
    pub sender_bot_id: Option<BotId>,
    /// Bot-to-bot hops from the world; bounded by `MAX_BOT_HOPS`.
    pub hops: u32,
    /// Public correlation of a receipt with the asked event.
    pub in_reply_to: Option<BotEventReplyRef>,

    // ── Receiver ──
    /// Who hears back when the delivery finishes; `None` when nobody
    /// listens.
    pub receiver: Option<EventReceiver>,

    // ── Outcome, written once when the delivery finishes ──
    pub outcome: Option<BotEventOutcome>,
    pub outcome_detail: Option<String>,
    /// Run that resolved the event, when one was started.
    pub run_id: Option<String>,
    pub resolved_at_ms: Option<i64>,
}

impl BotEventRecord {
    pub fn is_pending(&self) -> bool {
        self.outcome.is_none()
    }

    /// CAS ref of the receiver-bound tool declarations the routed session
    /// is created with (a chat conversation's `message_*` tools).
    pub fn tools_ref(&self) -> Option<&str> {
        self.receiver.as_ref().and_then(EventReceiver::tools_ref)
    }

    /// The asking bot and its logical session when the event asked for a
    /// `bot.reply` receipt.
    pub fn reply_route(&self) -> Option<(&BotId, Option<&RoutedSession>)> {
        match self.receiver.as_ref()? {
            EventReceiver::Bot { bot_id, session } => Some((bot_id, session.as_ref())),
            EventReceiver::Workflow { .. } => None,
        }
    }

    pub fn view(&self) -> BotEventView {
        BotEventView {
            seq: self.seq,
            event_id: self.event_id.clone(),
            trigger_id: self.trigger_id.clone(),
            kind: self.kind.clone(),
            summary: self.summary.clone(),
            occurred_at_ms: self.occurred_at_ms,
            received_at_ms: self.received_at_ms,
            document_ref: self.document_ref.clone(),
            prompt_ref: self.prompt_ref.clone(),
            session: self.session.as_ref().map(RoutedSession::view),
            media: self.media.clone(),
            sender_bot_id: self.sender_bot_id.clone(),
            hops: self.hops,
            in_reply_to: self.in_reply_to.clone(),
            outcome: self.outcome,
            outcome_detail: self.outcome_detail.clone(),
            run_id: self.run_id.clone(),
            resolved_at_ms: self.resolved_at_ms,
        }
    }
}

/// Result of an insert keyed by `(bot_id, event_id)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InsertBotEventOutcome {
    Inserted(BotEventRecord),
    /// The id was already stored; the stored row (with its own `seq` and
    /// refs) is returned so `#N` stays stable.
    Duplicate(BotEventRecord),
}

impl InsertBotEventOutcome {
    pub fn record(&self) -> &BotEventRecord {
        match self {
            Self::Inserted(record) | Self::Duplicate(record) => record,
        }
    }

    pub fn is_duplicate(&self) -> bool {
        matches!(self, Self::Duplicate(_))
    }
}

/// Keyset cursor of the event log: newest first, `(received_at_ms, seq)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotEventCursor {
    pub received_at_ms: i64,
    pub seq: u64,
}

/// Which events count toward a rate window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotEventRateScope<'a> {
    /// Events admitted through one trigger.
    Trigger {
        bot_id: &'a BotId,
        trigger_id: &'a BotTriggerId,
    },
    /// Events a bot sent (self or addressed), anywhere in the universe.
    Sender { sender_bot_id: &'a BotId },
}

/// Write-once outcome of a finished delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotEventOutcomeWrite {
    pub outcome: BotEventOutcome,
    pub detail: Option<String>,
    pub run_id: Option<String>,
    pub resolved_at_ms: i64,
}

#[async_trait]
pub trait BotEventStore: Send + Sync {
    /// Insert the row; a conflict on `(bot_id, event_id)` returns the stored
    /// row instead.
    async fn insert_bot_event(
        &self,
        record: BotEventRecord,
    ) -> Result<InsertBotEventOutcome, BotError>;

    /// Wake-failure compensation: remove a row this caller inserted.
    async fn delete_bot_event(&self, bot_id: &BotId, event_id: &str) -> Result<bool, BotError>;

    async fn read_bot_event_by_seq(
        &self,
        bot_id: &BotId,
        seq: u64,
    ) -> Result<BotEventRecord, BotError>;

    async fn read_bot_event(
        &self,
        bot_id: &BotId,
        event_id: &str,
    ) -> Result<BotEventRecord, BotError>;

    async fn read_bot_events(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
    ) -> Result<Vec<BotEventRecord>, BotError>;

    /// Newest first; `before` continues a page.
    async fn list_bot_events(
        &self,
        bot_id: &BotId,
        limit: usize,
        before: Option<BotEventCursor>,
    ) -> Result<Vec<BotEventRecord>, BotError>;

    async fn count_bot_events_since(
        &self,
        scope: BotEventRateScope<'_>,
        since_ms: i64,
    ) -> Result<u64, BotError>;

    /// Set the outcome columns of every listed event whose outcome is still
    /// null; returns how many rows changed.
    async fn record_bot_event_outcomes(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
        write: BotEventOutcomeWrite,
    ) -> Result<u64, BotError>;
}

/// Everything the bots subsystem needs from storage, in one bound.
pub trait BotRegistryStore: BotStore + BotTriggerStore + BotEventStore {}

impl<T: BotStore + BotTriggerStore + BotEventStore> BotRegistryStore for T {}
