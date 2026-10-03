//! The controller's state and every decision that needs no I/O: event
//! acceptance and dedupe, coalescing buffers, the UTC-day budget, dispatch
//! selection, resolve correlation, the outcome ladder, snapshot derivation,
//! emission classification, teardown sets, and the carry that crosses
//! continue-as-new. The workflow module drives activities and lanes around
//! this; nothing here awaits.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use api::{
    BotActiveDeliverySnapshot, BotBufferSnapshot, BotControllerSnapshot, BotControllerStatus,
    BotEventOutcome, BotRecentDeliverySnapshot, BotSessionKind, BotSessionSnapshot, BotSetupStatus,
    BotWhenBusy, ProfileId,
};
use bots::tools::{BOT_EVENT_RESOLVE_TOOL_ID, BOT_TOOLS_REVISION, is_pushed_tool};
use bots::{
    BotCoalesceParams, BotControllerConfig, BotEvent, RoutedSession, RoutedSessionClosePolicy, ids,
};
use harness::{
    BlobRef, EmissionBody, EmissionEnvelope, EmissionProducer, RunStatus, WorkflowToolInvocation,
};
use serde::{Deserialize, Serialize};

use super::super::{
    BOT_CONTINUE_AS_NEW_AFTER_EVENTS, BOT_DESCENDANT_REFRESH_INTERVAL, BOT_RECENT_DELIVERY_CAP,
    BOT_SEEN_ID_CAP, BotControllerSummary, BotToolInvocationRef,
};
use super::{BotControllerArgs, BotControllerCarry};

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
/// Retry pause after a rotation that found its session busy.
pub(super) const ROTATION_RETRY_DELAY_MS: i64 = 5_000;
const EVENT_ID_MAX_LEN: usize = 200;
const SESSION_ID_MAX_LEN: usize = 300;
const LABEL_MAX_LEN: usize = 200;
const COALESCE_KEY_MAX_LEN: usize = 400;

// ── Durable shapes (carried across continue-as-new) ─────────────────────────

/// One unit of work for a session: a single event or a coalesced batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotDelivery {
    pub id: String,
    pub events: Vec<BotEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<RoutedSession>,
    #[serde(default)]
    pub when_busy: BotWhenBusy,
}

impl BotDelivery {
    pub fn single(event: BotEvent) -> Self {
        Self {
            id: event.id.clone(),
            session: event.session.clone(),
            when_busy: event.when_busy.unwrap_or_default(),
            events: vec![event],
        }
    }

    /// `#N`s of the delivery's events.
    pub fn seqs(&self) -> Vec<u64> {
        self.events.iter().map(|event| event.seq).collect()
    }

    pub fn event_ids(&self) -> Vec<String> {
        self.events.iter().map(|event| event.id.clone()).collect()
    }

    /// The highest hop count among the delivery's events.
    pub fn hops(&self) -> u32 {
        self.events
            .iter()
            .map(|event| event.hops)
            .max()
            .unwrap_or(0)
    }

    pub fn notify_event_ids(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| event.notify)
            .map(|event| event.id.clone())
            .collect()
    }

    pub fn reply_event_ids(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| event.reply)
            .map(|event| event.id.clone())
            .collect()
    }
}

/// Events sharing a coalesce key, waiting for the window to close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoalesceBuffer {
    pub params: BotCoalesceParams,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<RoutedSession>,
    #[serde(default)]
    pub when_busy: BotWhenBusy,
    pub events: Vec<BotEvent>,
    pub first_at_ms: i64,
    pub last_at_ms: i64,
}

impl CoalesceBuffer {
    /// When the buffer flushes on its own: the debounce after the last
    /// event, bounded by the max wait after the first.
    pub fn flush_at_ms(&self) -> i64 {
        (self.last_at_ms + self.params.debounce_ms as i64)
            .min(self.first_at_ms + self.params.max_wait_ms as i64)
    }

    pub fn is_ripe(&self, now_ms: i64) -> bool {
        now_ms >= self.flush_at_ms()
    }

    fn into_delivery(self) -> BotDelivery {
        BotDelivery {
            id: ids::delivery_id(
                &self
                    .events
                    .iter()
                    .map(|event| event.id.clone())
                    .collect::<Vec<_>>(),
            ),
            events: self.events,
            session: self.session,
            when_busy: self.when_busy,
        }
    }
}

/// A routed (perKey / perEvent) session this controller created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSession {
    pub session_id: String,
    pub label: String,
    /// Preserved for self events and federation receipts across continuations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    pub kind: BotSessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at_ms: Option<i64>,
    /// Retention from the trigger; `Inherit` takes the bot's.
    #[serde(default)]
    pub close_policy: RoutedSessionClosePolicy,
    /// Receiver-bound tools the session was created with; a run that used
    /// one counts as handled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried_tool_ids: Vec<String>,
}

/// Insertion-ordered dedupe set, tail-capped so it stays bounded across
/// continue-as-new.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeenIds {
    order: VecDeque<String>,
    set: BTreeSet<String>,
    cap: usize,
}

impl SeenIds {
    pub fn with_cap(cap: usize) -> Self {
        Self {
            order: VecDeque::new(),
            set: BTreeSet::new(),
            cap,
        }
    }

    pub fn from_ids(ids: impl IntoIterator<Item = String>, cap: usize) -> Self {
        let mut seen = Self::with_cap(cap);
        for id in ids {
            seen.insert(id);
        }
        seen
    }

    /// `true` when the id was new.
    pub fn insert(&mut self, id: String) -> bool {
        if !self.set.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.set.remove(&evicted);
            }
        }
        true
    }

    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Ids in insertion order (the carry).
    pub fn tail(&self) -> Vec<String> {
        self.order.iter().cloned().collect()
    }
}

// ── Live-only shapes ────────────────────────────────────────────────────────

/// The run terminal a lane's run produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneTerminal {
    pub status: RunStatus,
    pub run_id: String,
    pub failure_message_ref: Option<BlobRef>,
}

/// The model's `bot_event_resolve` decision for a lane's run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneResolution {
    pub outcome: BotEventOutcome,
    pub summary: Option<String>,
}

/// One delivery lane: the delivery, the session it runs on, and what the
/// run reported so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveLane {
    pub delivery: BotDelivery,
    pub session_id: String,
    pub run_id: Option<String>,
    pub terminal: Option<LaneTerminal>,
    pub resolution: Option<LaneResolution>,
    pub started_at_ms: i64,
}

/// A pushed `bot_*` invocation waiting for its lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInvocation {
    pub invocation: WorkflowToolInvocation,
    pub holder_workflow_id: String,
}

/// What dispatch decided for one pending delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchAction {
    /// The target was free: the delivery owns the session's lane.
    Lane {
        delivery: BotDelivery,
        target: String,
    },
    /// The target has a started run: steer or append beside it.
    Sidecar {
        delivery: BotDelivery,
        target: String,
    },
}

/// The outcome a finished lane records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryOutcome {
    pub outcome: BotEventOutcome,
    pub summary: Option<String>,
    pub run_id: Option<String>,
}

/// What an emission means to this controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmissionDisposition {
    /// Another universe or another bot's session: dropped with a note.
    Foreign(String),
    /// A pushed `bot_*` invocation to answer.
    Invocation {
        session_id: String,
        invocation: Box<WorkflowToolInvocation>,
        holder_workflow_id: String,
    },
    /// A run terminal from one of the bot's sessions.
    Terminal {
        session_id: String,
        run_id: String,
        token: String,
        status: RunStatus,
        failure_message_ref: Option<BlobRef>,
    },
    /// Nothing for the controller (a pull tool, a resolution, a cancel).
    Ignore,
}

// ── The state ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ControllerState {
    pub config: BotControllerConfig,
    pub main_generation: u32,
    pub tools_revision: Option<u32>,
    pub pending_deliveries: Vec<BotDelivery>,
    pub buffers: BTreeMap<String, CoalesceBuffer>,
    pub recent_deliveries: Vec<BotRecentDeliverySnapshot>,
    pub seen_event_ids: SeenIds,
    pub seen_emission_ids: SeenIds,
    pub handled_invocation_ids: SeenIds,
    pub emission_inbox: VecDeque<EmissionEnvelope>,
    pub pending_invocations: Vec<PendingInvocation>,
    pub extra_sessions: Vec<ManagedSession>,
    /// Session log cursor per session (main generations included).
    pub session_cursors: BTreeMap<String, u64>,
    /// Routed base id → generation, bumped each time that session closes.
    pub session_generations: BTreeMap<String, u32>,
    pub rotation_requests: BTreeSet<String>,
    pub active_by_session: BTreeMap<String, ActiveLane>,
    pub sidecar_by_session: BTreeSet<String>,
    pub applied_profile_id: Option<ProfileId>,
    pub applied_profile_revision: Option<u64>,
    pub session_ready: bool,
    pub events_processed: u64,
    pub duplicate_events: u64,
    pub duplicate_emissions: u64,
    pub run_day: String,
    pub runs_today: u32,
    pub descendants_today: u32,
    pub budget_roots: BTreeSet<String>,
    pub descendants_refreshed_at_ms: i64,
    pub descendants_refreshed_processed: Option<u64>,
    /// Bumped whenever a lane changes shared state, so a parked loop
    /// re-evaluates deadlines and dispatch.
    pub lane_tick: u64,
    /// Bumped by every accepted event so the loop schedules buffer timers.
    pub event_tick: u64,
    pub rotation_retry_at_ms: Option<i64>,
    pub setup_status: BotSetupStatus,
    pub config_dirty: bool,
    pub rename_dirty: bool,
    pub closing: bool,
    pub closed_done: bool,
    pub last_error: Option<String>,
    /// The last workflow time the loop or a handler observed; the query
    /// reads it because it has no clock of its own.
    pub clock_ms: i64,
}

impl ControllerState {
    pub fn new(args: BotControllerArgs, now_ms: i64) -> Self {
        let BotControllerArgs { config, carry } = args;
        let carry = carry.unwrap_or_default();
        let pending_deliveries = carry.pending_deliveries;
        let seen_event_ids = if carry.seen_event_ids.is_empty() {
            SeenIds::from_ids(
                pending_deliveries.iter().flat_map(BotDelivery::event_ids),
                BOT_SEEN_ID_CAP,
            )
        } else {
            SeenIds::from_ids(carry.seen_event_ids, BOT_SEEN_ID_CAP)
        };
        Self {
            config,
            main_generation: carry.main_generation.max(1),
            tools_revision: carry.tools_revision,
            pending_deliveries,
            buffers: carry.buffers,
            recent_deliveries: carry.recent_deliveries,
            seen_event_ids,
            seen_emission_ids: SeenIds::from_ids(carry.seen_emission_ids, BOT_SEEN_ID_CAP),
            handled_invocation_ids: SeenIds::from_ids(
                carry.handled_invocation_ids,
                BOT_SEEN_ID_CAP,
            ),
            emission_inbox: VecDeque::new(),
            pending_invocations: Vec::new(),
            extra_sessions: carry.extra_sessions,
            session_cursors: carry.session_cursors,
            session_generations: carry.session_generations,
            rotation_requests: carry.rotation_requests.into_iter().collect(),
            active_by_session: BTreeMap::new(),
            sidecar_by_session: BTreeSet::new(),
            applied_profile_id: carry.applied_profile_id,
            applied_profile_revision: carry.applied_profile_revision,
            session_ready: carry.session_ready,
            events_processed: carry.events_processed,
            duplicate_events: carry.duplicate_events,
            duplicate_emissions: carry.duplicate_emissions,
            run_day: carry.run_day.unwrap_or_else(|| utc_day(now_ms)),
            runs_today: carry.runs_today,
            descendants_today: carry.descendants_today,
            budget_roots: carry.budget_roots.into_iter().collect(),
            descendants_refreshed_at_ms: 0,
            descendants_refreshed_processed: None,
            lane_tick: 0,
            event_tick: 0,
            rotation_retry_at_ms: None,
            setup_status: BotSetupStatus::Initializing,
            config_dirty: true,
            rename_dirty: false,
            closing: false,
            closed_done: false,
            last_error: None,
            clock_ms: now_ms,
        }
    }

    /// Everything a successor execution needs; the live lane state is
    /// quiescent by the time this is taken.
    pub fn carry(&self) -> BotControllerCarry {
        BotControllerCarry {
            pending_deliveries: self.pending_deliveries.clone(),
            buffers: self.buffers.clone(),
            recent_deliveries: self.recent_deliveries.clone(),
            seen_event_ids: self.seen_event_ids.tail(),
            seen_emission_ids: self.seen_emission_ids.tail(),
            handled_invocation_ids: self.handled_invocation_ids.tail(),
            session_cursors: self.session_cursors.clone(),
            applied_profile_id: self.applied_profile_id.clone(),
            applied_profile_revision: self.applied_profile_revision,
            session_ready: self.session_ready,
            events_processed: 0,
            duplicate_events: self.duplicate_events,
            duplicate_emissions: self.duplicate_emissions,
            run_day: Some(self.run_day.clone()),
            runs_today: self.runs_today,
            descendants_today: self.descendants_today,
            budget_roots: self.budget_roots.iter().cloned().collect(),
            extra_sessions: self.extra_sessions.clone(),
            session_generations: self.session_generations.clone(),
            rotation_requests: self.rotation_requests.iter().cloned().collect(),
            main_generation: self.main_generation,
            tools_revision: self.tools_revision,
        }
    }

    // ── Identity and labels ─────────────────────────────────────────────

    pub fn main_session_id(&self) -> String {
        ids::bot_main_session_id(&self.config.bot_id, self.main_generation)
    }

    /// Session display names carry the label, never the id.
    pub fn bot_label(&self) -> String {
        format!(
            "bot {}",
            self.config
                .display_name
                .as_deref()
                .unwrap_or(self.config.bot_id.as_str())
        )
    }

    pub fn routed_label(&self, label: &str) -> String {
        format!("{} · {label}", self.bot_label())
    }

    /// Current session id for a routed base, accounting for closed
    /// generations.
    pub fn resolve_routed_session_id(&self, base: &str) -> String {
        let generation = self.session_generations.get(base).copied().unwrap_or(1);
        ids::routed_session_generation_id(base, generation)
    }

    pub fn bump_routed_generation(&mut self, base: &str) -> u32 {
        let next = self.session_generations.get(base).copied().unwrap_or(1) + 1;
        self.session_generations.insert(base.to_owned(), next);
        next
    }

    pub fn target_of(&self, delivery: &BotDelivery) -> String {
        match &delivery.session {
            None => self.main_session_id(),
            Some(session) => self.resolve_routed_session_id(&session.session_id),
        }
    }

    pub fn close_requested(&self) -> bool {
        self.config.closed
    }

    pub fn record_error(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
    }

    // ── Signals ─────────────────────────────────────────────────────────

    /// The `bot_event` signal: validate, dedupe, then queue a delivery or
    /// fold into the coalesce buffer of the event's key.
    pub fn accept_event(&mut self, event: BotEvent, now_ms: i64) -> Result<(), String> {
        validate_event(&event)?;
        self.event_tick += 1;
        if !self.seen_event_ids.insert(event.id.clone()) {
            self.duplicate_events += 1;
            return Ok(());
        }
        let Some(params) = event.coalesce.clone() else {
            self.pending_deliveries.push(BotDelivery::single(event));
            return Ok(());
        };
        let key = params.key.clone();
        let buffer = self
            .buffers
            .entry(key.clone())
            .or_insert_with(|| CoalesceBuffer {
                params: params.clone(),
                session: event.session.clone(),
                when_busy: event.when_busy.unwrap_or_default(),
                events: Vec::new(),
                first_at_ms: now_ms,
                last_at_ms: now_ms,
            });
        buffer.events.push(event);
        buffer.last_at_ms = now_ms;
        buffer.params = params;
        if buffer.events.len() as u32 >= buffer.params.max_count {
            self.flush_buffer(&key);
        }
        Ok(())
    }

    /// The `bot_config` signal: the identity is fixed at start, a display
    /// name change is label-only, anything else reconciles the main
    /// session.
    pub fn accept_config(&mut self, next: BotControllerConfig) -> Result<(), String> {
        if next.universe_id != self.config.universe_id || next.bot_id != self.config.bot_id {
            return Err(format!(
                "bot identity cannot change: got {}/{}, controller is {}/{}",
                next.universe_id, next.bot_id, self.config.universe_id, self.config.bot_id
            ));
        }
        if next.display_name != self.config.display_name {
            self.rename_dirty = true;
        }
        self.config = next;
        self.config_dirty = true;
        Ok(())
    }

    pub fn request_rotation(&mut self, session_id: String) {
        if session_id.is_empty() {
            return;
        }
        self.rotation_requests.insert(session_id);
        self.rotation_retry_at_ms = None;
        self.lane_tick += 1;
    }

    pub fn queue_emission(&mut self, envelope: EmissionEnvelope) {
        self.emission_inbox.push_back(envelope);
    }

    // ── Coalescing ──────────────────────────────────────────────────────

    pub fn flush_buffer(&mut self, key: &str) {
        let Some(buffer) = self.buffers.remove(key) else {
            return;
        };
        if buffer.events.is_empty() {
            return;
        }
        self.pending_deliveries.push(buffer.into_delivery());
    }

    pub fn flush_ripe_buffers(&mut self, now_ms: i64) {
        let ripe: Vec<String> = self
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.is_ripe(now_ms))
            .map(|(key, _)| key.clone())
            .collect();
        for key in ripe {
            self.flush_buffer(&key);
        }
    }

    pub fn next_buffer_deadline(&self) -> Option<i64> {
        self.buffers.values().map(CoalesceBuffer::flush_at_ms).min()
    }

    // ── Sessions ────────────────────────────────────────────────────────

    pub fn is_session_busy(&self, session_id: &str) -> bool {
        self.active_by_session.contains_key(session_id)
            || self.sidecar_by_session.contains(session_id)
    }

    pub fn main_session_free(&self) -> bool {
        !self.is_session_busy(&self.main_session_id())
    }

    pub fn extra_session(&self, session_id: &str) -> Option<&ManagedSession> {
        self.extra_sessions
            .iter()
            .find(|session| session.session_id == session_id)
    }

    pub fn extra_session_mut(&mut self, session_id: &str) -> Option<&mut ManagedSession> {
        self.extra_sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
    }

    /// Record a routed event against an already-known session. The activity
    /// stamp drives both idle close and least-recently-used cap eviction.
    pub fn observe_routed_session(
        &mut self,
        session_id: &str,
        routed: &RoutedSession,
        observed_at_ms: i64,
    ) -> bool {
        let Some(known) = self.extra_session_mut(session_id) else {
            return false;
        };
        if routed.close_policy != RoutedSessionClosePolicy::Inherit {
            known.close_policy = routed.close_policy;
        }
        if routed.session_key.is_some() {
            known.session_key = routed.session_key.clone();
        }
        known.last_active_at_ms = Some(observed_at_ms);
        true
    }

    /// Effective idle-close duration of a routed session: the trigger's, else the
    /// bot's; `None` never closes.
    pub fn session_close_after_ms(&self, session: &ManagedSession) -> Option<u64> {
        match session.close_policy {
            RoutedSessionClosePolicy::Inherit => self
                .config
                .routed_session_close_after_ms
                .filter(|duration| *duration > 0),
            RoutedSessionClosePolicy::Never => None,
            RoutedSessionClosePolicy::After { ms } => (ms > 0).then_some(ms),
        }
    }

    pub fn session_expiry_ms(&self, session: &ManagedSession) -> Option<i64> {
        let close_after_ms = self.session_close_after_ms(session)?;
        Some(session.last_active_at_ms.unwrap_or(0) + close_after_ms as i64)
    }

    pub fn next_idle_close_deadline(&self) -> Option<i64> {
        self.extra_sessions
            .iter()
            .filter(|session| !self.is_session_busy(&session.session_id))
            .filter_map(|session| self.session_expiry_ms(session))
            .min()
    }

    /// Routed sessions idle past their close window.
    pub fn expired_sessions(&self, now_ms: i64) -> Vec<String> {
        self.extra_sessions
            .iter()
            .filter(|session| !self.is_session_busy(&session.session_id))
            .filter(|session| {
                self.session_expiry_ms(session)
                    .is_some_and(|expiry| expiry <= now_ms)
            })
            .map(|session| session.session_id.clone())
            .collect()
    }

    pub fn touch_session(&mut self, session_id: &str, now_ms: i64) {
        if let Some(session) = self.extra_session_mut(session_id) {
            session.last_active_at_ms = Some(now_ms);
        }
    }

    /// Forget a closed routed session and advance its base's generation.
    pub fn forget_routed_session(&mut self, session_id: &str) {
        self.extra_sessions
            .retain(|session| session.session_id != session_id);
        self.session_cursors.remove(session_id);
        let base = ids::routed_session_base(session_id).to_owned();
        self.bump_routed_generation(&base);
    }

    /// The idlest routed session that is not busy: the eviction candidate
    /// when the tracked set is over its cap.
    pub fn idlest_free_session(&self, excluding: &str) -> Option<String> {
        self.extra_sessions
            .iter()
            .filter(|session| session.session_id != excluding)
            .filter(|session| !self.is_session_busy(&session.session_id))
            .min_by_key(|session| session.last_active_at_ms.unwrap_or(0))
            .map(|session| session.session_id.clone())
    }

    /// Rotate the main session to its next generation: the successor does
    /// not exist until the next reconcile creates it.
    pub fn rotate_main_session(&mut self, reset_ready: bool) {
        self.main_generation += 1;
        self.applied_profile_id = None;
        self.applied_profile_revision = None;
        if reset_ready {
            self.tools_revision = None;
            self.session_ready = false;
            self.config_dirty = true;
            self.setup_status = BotSetupStatus::Initializing;
        }
    }

    pub fn mark_session_ready(&mut self, profile_revision: u64) {
        self.session_ready = true;
        self.applied_profile_id = Some(self.config.profile_id.clone());
        self.applied_profile_revision = Some(profile_revision);
        self.tools_revision = Some(BOT_TOOLS_REVISION);
        self.config_dirty = false;
        self.last_error = None;
        self.setup_status = BotSetupStatus::Ready;
    }

    pub fn mark_session_degraded(&mut self, message: String) {
        self.last_error = Some(message);
        self.setup_status = BotSetupStatus::Degraded;
        self.config_dirty = false;
        self.session_ready = false;
    }

    /// The profile revision to hand `ensure_session` for the main session:
    /// only when the applied profile is still the configured one.
    pub fn applied_revision_for_ensure(&self) -> Option<u64> {
        (self.applied_profile_id.as_ref() == Some(&self.config.profile_id))
            .then_some(self.applied_profile_revision)
            .flatten()
    }

    pub fn cursor_for(&self, session_id: &str) -> u64 {
        self.session_cursors.get(session_id).copied().unwrap_or(0)
    }

    pub fn set_cursor(&mut self, session_id: &str, seq: u64) {
        self.session_cursors.insert(session_id.to_owned(), seq);
    }

    // ── Budget ──────────────────────────────────────────────────────────

    pub fn roll_budget_day(&mut self, now_ms: i64) {
        let today = utc_day(now_ms);
        if today != self.run_day {
            self.run_day = today;
            self.runs_today = 0;
            self.descendants_today = 0;
            self.budget_roots.clear();
            self.descendants_refreshed_at_ms = 0;
            self.descendants_refreshed_processed = None;
        }
    }

    /// Runs already started today, sub-agent sessions delegated today, and
    /// lanes about to start a run.
    pub fn reserved_runs(&self) -> u32 {
        let pending_starts = self
            .active_by_session
            .values()
            .filter(|lane| lane.run_id.is_none() && lane.delivery.when_busy != BotWhenBusy::Append)
            .count() as u32;
        self.runs_today + self.descendants_today + pending_starts
    }

    /// Rolls the day first (the loop's variant).
    pub fn budget_exhausted(&mut self, now_ms: i64) -> bool {
        self.roll_budget_day(now_ms);
        self.budget_exhausted_view(now_ms)
    }

    /// Pure variant for the query handler and wake conditions: a stale day
    /// is never exhausted.
    pub fn budget_exhausted_view(&self, now_ms: i64) -> bool {
        let Some(runs_per_day) = self.config.runs_per_day else {
            return false;
        };
        if utc_day(now_ms) != self.run_day {
            return false;
        }
        self.reserved_runs() >= runs_per_day
    }

    pub fn day_start_ms(&self) -> i64 {
        day_start_ms(&self.run_day)
    }

    pub fn descendants_refresh_due(&self, now_ms: i64) -> bool {
        if self.config.runs_per_day.is_none() {
            return false;
        }
        if self.descendants_refreshed_processed != Some(self.events_processed) {
            return true;
        }
        !self.active_by_session.is_empty()
            && now_ms - self.descendants_refreshed_at_ms
                >= BOT_DESCENDANT_REFRESH_INTERVAL.as_millis() as i64
    }

    /// Roots whose delegation trees count today: the main session and
    /// every routed session.
    pub fn budget_root_ids(&mut self) -> Vec<String> {
        self.budget_roots.insert(self.main_session_id());
        for session in &self.extra_sessions {
            self.budget_roots.insert(session.session_id.clone());
        }
        self.budget_roots.iter().cloned().collect()
    }

    // ── Dispatch ────────────────────────────────────────────────────────

    /// Whether the loop has a delivery it could hand to a session now.
    pub fn dispatchable(&self, now_ms: i64) -> bool {
        if !self.config.enabled
            || self.close_requested()
            || !self.session_ready
            || self.config_dirty
            || self.pending_deliveries.is_empty()
            || self.budget_exhausted_view(now_ms)
        {
            return false;
        }
        self.pending_deliveries.iter().any(|delivery| {
            let target = self.target_of(delivery);
            if self.rotation_requests.contains(&target) {
                return false;
            }
            match self.active_by_session.get(&target) {
                None => true,
                Some(active) => {
                    active.run_id.is_some()
                        && delivery.when_busy != BotWhenBusy::Queue
                        && !self.sidecar_by_session.contains(&target)
                }
            }
        })
    }

    /// Hand every runnable pending delivery to a lane or a sidecar, FIFO.
    /// The caller checks the enabled/ready/dirty preconditions; the budget
    /// is re-checked per delivery because each lane reserves a run.
    pub fn dispatch(&mut self, now_ms: i64) -> Vec<DispatchAction> {
        let mut actions = Vec::new();
        let mut index = 0;
        while index < self.pending_deliveries.len() {
            if self.budget_exhausted(now_ms) {
                break;
            }
            let target = self.target_of(&self.pending_deliveries[index]);
            if self.rotation_requests.contains(&target) {
                index += 1;
                continue;
            }
            if let Some(occupied) = self.active_by_session.get(&target) {
                let delivery = &self.pending_deliveries[index];
                if occupied.run_id.is_some()
                    && delivery.when_busy != BotWhenBusy::Queue
                    && !self.sidecar_by_session.contains(&target)
                {
                    let delivery = self.pending_deliveries.remove(index);
                    self.sidecar_by_session.insert(target.clone());
                    actions.push(DispatchAction::Sidecar { delivery, target });
                    continue;
                }
                index += 1;
                continue;
            }
            let delivery = self.pending_deliveries.remove(index);
            self.active_by_session.insert(
                target.clone(),
                ActiveLane {
                    delivery: delivery.clone(),
                    session_id: target.clone(),
                    run_id: None,
                    terminal: None,
                    resolution: None,
                    started_at_ms: now_ms,
                },
            );
            actions.push(DispatchAction::Lane { delivery, target });
        }
        actions
    }

    /// Put a delivery back at the front of the queue (a lost start race, a
    /// sidecar whose run finished underneath it).
    pub fn requeue_front(&mut self, delivery: BotDelivery) {
        self.pending_deliveries.insert(0, delivery);
    }

    /// Move a lane to the session id its routed target rotated to during
    /// ensure, so terminals and busy checks find it.
    pub fn rekey_lane(&mut self, from: &str, to: &str) {
        if let Some(mut lane) = self.active_by_session.remove(from) {
            lane.session_id = to.to_owned();
            self.active_by_session.insert(to.to_owned(), lane);
        }
    }

    pub fn mark_lane_started(&mut self, session_id: &str, run_id: &str, now_ms: i64) {
        if let Some(lane) = self.active_by_session.get_mut(session_id) {
            lane.run_id = Some(run_id.to_owned());
        }
        self.roll_budget_day(now_ms);
        self.runs_today += 1;
        self.lane_tick += 1;
    }

    /// Terminals are matched by the delivery's token, never by session: a
    /// lane that rotated its session still owns its run.
    pub fn attach_terminal(&mut self, token: &str, terminal: LaneTerminal) -> bool {
        let mut attached = false;
        for lane in self.active_by_session.values_mut() {
            if ids::delivery_terminal_token(&lane.delivery.id) == token {
                lane.terminal = Some(terminal.clone());
                attached = true;
            }
        }
        if attached {
            self.lane_tick += 1;
        }
        attached
    }

    pub fn set_lane_resolution(&mut self, session_id: &str, resolution: LaneResolution) {
        if let Some(lane) = self.active_by_session.get_mut(session_id) {
            lane.resolution = Some(resolution);
        }
    }

    pub fn lane_has_terminal(&self, session_id: &str) -> bool {
        self.active_by_session
            .get(session_id)
            .is_some_and(|lane| lane.terminal.is_some())
    }

    // ── Finishing deliveries ────────────────────────────────────────────

    pub fn remember_delivery(&mut self, recent: BotRecentDeliverySnapshot) {
        self.recent_deliveries.push(recent);
        if self.recent_deliveries.len() > BOT_RECENT_DELIVERY_CAP {
            let excess = self.recent_deliveries.len() - BOT_RECENT_DELIVERY_CAP;
            self.recent_deliveries.drain(0..excess);
        }
        self.events_processed += 1;
    }

    pub fn release_lane(&mut self, session_id: &str, now_ms: i64) {
        self.active_by_session.remove(session_id);
        self.touch_session(session_id, now_ms);
        self.lane_tick += 1;
    }

    pub fn release_sidecar(&mut self, session_id: &str, now_ms: i64) {
        self.sidecar_by_session.remove(session_id);
        self.touch_session(session_id, now_ms);
        self.lane_tick += 1;
    }

    // ── Emissions ───────────────────────────────────────────────────────

    /// Dedupe by emission id, then classify. `None` is a duplicate.
    pub fn classify_emission(&mut self, envelope: EmissionEnvelope) -> Option<EmissionDisposition> {
        if !self
            .seen_emission_ids
            .insert(envelope.emission_id.as_str().to_owned())
        {
            self.duplicate_emissions += 1;
            return None;
        }
        let disposition = classify_emission(&self.config, &self.main_session_id(), envelope);
        match disposition {
            EmissionDisposition::Invocation { ref invocation, .. } => {
                if !self
                    .handled_invocation_ids
                    .insert(invocation.invocation_id.as_str().to_owned())
                {
                    return Some(EmissionDisposition::Ignore);
                }
                Some(disposition)
            }
            other => Some(other),
        }
    }

    /// What the tool activity may show the model, plus the invoking
    /// delivery's federation context.
    pub fn controller_summary(
        &self,
        invoking_session_id: &str,
        now_ms: i64,
    ) -> BotControllerSummary {
        let hops = self
            .active_by_session
            .get(invoking_session_id)
            .map(|lane| lane.delivery.hops())
            .unwrap_or(0);
        let routed_session = self
            .extra_session(invoking_session_id)
            .map(|session| RoutedSession {
                session_id: ids::routed_session_base(&session.session_id).to_owned(),
                label: session.label.clone(),
                session_key: session.session_key.clone(),
                close_policy: session.close_policy,
            });
        BotControllerSummary {
            snapshot: self.snapshot(now_ms),
            hops,
            routed_session,
        }
    }

    // ── Loop control ────────────────────────────────────────────────────

    /// Whether a parked loop should wake, given the ticks it observed when
    /// it parked.
    pub fn wake_ready(&self, lane_tick: u64, event_tick: u64) -> bool {
        self.close_requested()
            || !self.emission_inbox.is_empty()
            || !self.pending_invocations.is_empty()
            || (self.config_dirty && self.main_session_free())
            || self.lane_tick != lane_tick
            || self.event_tick != event_tick
            || self.dispatchable(self.clock_ms)
    }

    /// The earliest time-driven reason to wake: a buffer flush, a routed-session idle-close
    /// expiry, a rotation retry, the UTC day boundary while budget-parked,
    /// or the descendant refresh while a run is in flight.
    pub fn wake_deadline(&self, now_ms: i64) -> Option<i64> {
        let mut deadlines = vec![
            self.next_buffer_deadline(),
            self.next_idle_close_deadline(),
            self.rotation_retry_at_ms,
        ];
        if !self.pending_deliveries.is_empty()
            && self.config.runs_per_day.is_some()
            && self.budget_exhausted_view(now_ms)
        {
            deadlines.push(Some(now_ms + ms_until_next_utc_day(now_ms)));
        }
        if self.config.runs_per_day.is_some() && !self.active_by_session.is_empty() {
            deadlines.push(Some(
                self.descendants_refreshed_at_ms
                    + BOT_DESCENDANT_REFRESH_INTERVAL.as_millis() as i64,
            ));
        }
        deadlines.into_iter().flatten().min()
    }

    /// Continue-as-new needs a quiet controller: nothing in the inbox, no
    /// lane, and the processed-event threshold reached (or the server's
    /// suggestion).
    pub fn can_continue_as_new(&self, suggested: bool, lanes_active: bool) -> bool {
        self.emission_inbox.is_empty()
            && self.pending_invocations.is_empty()
            && self.active_by_session.is_empty()
            && self.sidecar_by_session.is_empty()
            && !lanes_active
            && !self.close_requested()
            && (self.events_processed >= BOT_CONTINUE_AS_NEW_AFTER_EVENTS || suggested)
    }

    // ── Teardown ────────────────────────────────────────────────────────

    /// Every event not yet delivered: pending, buffered, and active.
    pub fn teardown_event_ids(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut ids = Vec::new();
        let candidates = self
            .pending_deliveries
            .iter()
            .flat_map(BotDelivery::event_ids)
            .chain(
                self.buffers
                    .values()
                    .flat_map(|buffer| buffer.events.iter().map(|event| event.id.clone())),
            )
            .chain(
                self.active_by_session
                    .values()
                    .flat_map(|lane| lane.delivery.event_ids()),
            );
        for id in candidates {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
        ids
    }

    /// Every session this controller knows: main generations and routed
    /// sessions.
    pub fn teardown_sessions(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut sessions = Vec::new();
        let mains = (1..=self.main_generation)
            .map(|generation| ids::bot_main_session_id(&self.config.bot_id, generation));
        let extras = self
            .extra_sessions
            .iter()
            .map(|session| session.session_id.clone());
        for id in mains.chain(extras) {
            if seen.insert(id.clone()) {
                sessions.push(id);
            }
        }
        sessions
    }

    // ── Snapshot ────────────────────────────────────────────────────────

    pub fn controller_status(&self, now_ms: i64) -> BotControllerStatus {
        if self.closed_done {
            return BotControllerStatus::Closed;
        }
        if self.closing {
            return BotControllerStatus::Closing;
        }
        match self.setup_status {
            BotSetupStatus::Initializing => return BotControllerStatus::Initializing,
            BotSetupStatus::Degraded => return BotControllerStatus::Degraded,
            BotSetupStatus::Ready => {}
        }
        if !self.active_by_session.is_empty() {
            return BotControllerStatus::DeliveringEvent;
        }
        if !self.pending_deliveries.is_empty() && self.budget_exhausted_view(now_ms) {
            return BotControllerStatus::BudgetExhausted;
        }
        BotControllerStatus::Idle
    }

    pub fn snapshot(&self, now_ms: i64) -> BotControllerSnapshot {
        let main_session_id = self.main_session_id();
        let mut sessions = vec![BotSessionSnapshot {
            session_id: main_session_id.clone(),
            label: "main".to_owned(),
            kind: BotSessionKind::Main,
            generation: self.main_generation,
            last_active_at_ms: None,
            busy: self.is_session_busy(&main_session_id),
        }];
        sessions.extend(self.extra_sessions.iter().map(|session| {
            BotSessionSnapshot {
                session_id: session.session_id.clone(),
                label: session.label.clone(),
                kind: session.kind,
                generation: self
                    .session_generations
                    .get(ids::routed_session_base(&session.session_id))
                    .copied()
                    .unwrap_or(1),
                last_active_at_ms: session.last_active_at_ms,
                busy: self.is_session_busy(&session.session_id),
            }
        }));
        BotControllerSnapshot {
            controller_status: self.controller_status(now_ms),
            setup_status: self.setup_status,
            enabled: self.config.enabled,
            closed: self.config.closed,
            main_session_id,
            sessions,
            pending_deliveries: self.pending_deliveries.len() as u32,
            buffers: self
                .buffers
                .iter()
                .map(|(key, buffer)| BotBufferSnapshot {
                    key: key.clone(),
                    seqs: buffer.events.iter().map(|event| event.seq).collect(),
                    first_at_ms: buffer.first_at_ms,
                    last_at_ms: buffer.last_at_ms,
                    flush_at_ms: buffer.flush_at_ms(),
                })
                .collect(),
            active_deliveries: self
                .active_by_session
                .values()
                .map(|lane| BotActiveDeliverySnapshot {
                    delivery_id: lane.delivery.id.clone(),
                    seqs: lane.delivery.seqs(),
                    session_id: lane.session_id.clone(),
                    run_id: lane.run_id.clone(),
                    started_at_ms: lane.started_at_ms,
                })
                .collect(),
            recent_deliveries: self.recent_deliveries.clone(),
            run_day: Some(self.run_day.clone()),
            runs_today: self.runs_today,
            descendants_today: self.descendants_today,
            events_processed: self.events_processed,
            duplicate_events: self.duplicate_events,
            applied_profile_revision: self.applied_profile_revision,
            last_error: self.last_error.clone(),
        }
    }
}

// ── Pure helpers ────────────────────────────────────────────────────────────

/// Minimal shape check of an event signal; the row is authoritative.
pub fn validate_event(event: &BotEvent) -> Result<(), String> {
    if event.id.is_empty() || event.id.len() > EVENT_ID_MAX_LEN {
        return Err("invalid bot event id".to_owned());
    }
    if event.document_ref.is_empty() {
        return Err("invalid bot event document ref".to_owned());
    }
    if let Some(session) = &event.session {
        if session.session_id.is_empty() || session.session_id.len() > SESSION_ID_MAX_LEN {
            return Err("invalid bot event session id".to_owned());
        }
        if session.label.is_empty() || session.label.len() > LABEL_MAX_LEN {
            return Err("invalid bot event session label".to_owned());
        }
    }
    if let Some(coalesce) = &event.coalesce {
        if coalesce.key.is_empty() || coalesce.key.len() > COALESCE_KEY_MAX_LEN {
            return Err("invalid coalesce key".to_owned());
        }
        if coalesce.max_count == 0 {
            return Err("invalid coalesce maxCount".to_owned());
        }
        if coalesce.max_wait_ms < coalesce.debounce_ms {
            return Err("coalesce maxWaitMs must cover debounceMs".to_owned());
        }
    }
    Ok(())
}

/// Which emissions the controller acts on. Session producers must be this
/// bot's sessions in this universe; a workflow producer stands for the
/// main session.
pub fn classify_emission(
    config: &BotControllerConfig,
    main_session_id: &str,
    envelope: EmissionEnvelope,
) -> EmissionDisposition {
    let producer_session = match &envelope.producer {
        EmissionProducer::Session {
            universe_id,
            session_id,
            ..
        } => {
            if *universe_id != config.universe_id
                || !ids::is_bot_session(&config.bot_id, session_id.as_str())
            {
                return EmissionDisposition::Foreign(format!(
                    "emission {} does not belong to this bot's sessions (producer {}/{})",
                    envelope.emission_id, universe_id, session_id
                ));
            }
            session_id.as_str().to_owned()
        }
        EmissionProducer::Workflow {
            universe_id,
            workflow_id,
        } => {
            if *universe_id != config.universe_id {
                return EmissionDisposition::Foreign(format!(
                    "emission {} comes from another universe (producer workflow {})",
                    envelope.emission_id, workflow_id
                ));
            }
            main_session_id.to_owned()
        }
    };
    match envelope.body {
        EmissionBody::ToolInvocation {
            invocation,
            holder_workflow_id,
        } => {
            if !is_pushed_tool(invocation.tool_id.as_str()) {
                return EmissionDisposition::Ignore;
            }
            EmissionDisposition::Invocation {
                session_id: invocation.session_id.as_str().to_owned(),
                invocation: Box::new(invocation),
                holder_workflow_id,
            }
        }
        EmissionBody::RunTerminal {
            token,
            run_id,
            status,
            failure_message_ref,
            ..
        } => EmissionDisposition::Terminal {
            session_id: producer_session,
            run_id: format!("run_{}", run_id.as_u64()),
            token,
            status,
            failure_message_ref,
        },
        EmissionBody::SourceResolution { .. } | EmissionBody::InvocationCancellation { .. } => {
            EmissionDisposition::Ignore
        }
    }
}

/// The `bot_event_resolve` invocations of one run, in log order: the lane
/// runs exactly one delivery per run, so any of them decides that delivery
/// and the last one wins.
pub fn resolve_invocations_for_run<'a>(
    invocations: &'a [BotToolInvocationRef],
    run_id: &str,
) -> Vec<&'a BotToolInvocationRef> {
    invocations
        .iter()
        .filter(|invocation| invocation.run_id == run_id)
        .filter(|invocation| invocation.tool_id == BOT_EVENT_RESOLVE_TOOL_ID)
        .collect()
}

/// A run that answered through a carried tool (a chat reply) handled its
/// delivery without a resolve call.
pub fn used_carried_tool(
    invocations: &[BotToolInvocationRef],
    run_id: &str,
    carried_tool_ids: &[String],
) -> bool {
    !carried_tool_ids.is_empty()
        && invocations.iter().any(|invocation| {
            invocation.run_id == run_id && carried_tool_ids.contains(&invocation.tool_id)
        })
}

/// The outcome ladder of a lane that started a run.
pub fn delivery_outcome(
    terminal: Option<&LaneTerminal>,
    resolution: Option<&LaneResolution>,
    started_run_id: Option<&str>,
) -> DeliveryOutcome {
    let Some(terminal) = terminal else {
        return DeliveryOutcome {
            outcome: BotEventOutcome::RunFailed,
            summary: Some("timed out waiting for the run terminal".to_owned()),
            run_id: started_run_id.map(str::to_owned),
        };
    };
    let run_id = Some(
        started_run_id
            .map(str::to_owned)
            .unwrap_or_else(|| terminal.run_id.clone()),
    );
    if terminal.status != RunStatus::Completed {
        return DeliveryOutcome {
            outcome: BotEventOutcome::RunFailed,
            summary: Some(format!("run ended {}", run_status_label(terminal.status))),
            run_id,
        };
    }
    match resolution {
        Some(resolution) => DeliveryOutcome {
            outcome: resolution.outcome,
            summary: resolution.summary.clone(),
            run_id,
        },
        None => DeliveryOutcome {
            outcome: BotEventOutcome::Unresolved,
            summary: None,
            run_id,
        },
    }
}

pub fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Active => "active",
        RunStatus::Parked => "parked",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

/// `YYYY-MM-DD` of a UTC instant.
pub fn utc_day(now_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now_ms)
        .map(|time| time.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_owned())
}

/// Midnight UTC of a `YYYY-MM-DD` day, in ms.
pub fn day_start_ms(day: &str) -> i64 {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc().timestamp_millis())
        .unwrap_or(0)
}

/// Until the next UTC midnight, at least a second.
pub fn ms_until_next_utc_day(now_ms: i64) -> i64 {
    let next = (now_ms.div_euclid(MS_PER_DAY) + 1) * MS_PER_DAY;
    (next - now_ms).max(1_000)
}

#[cfg(test)]
mod tests {
    use api::{BotId, ProfileId};
    use harness::{EventSeq, PromiseId, PromiseResolution, RunId, SessionId};
    use uuid::Uuid;

    use super::*;

    const UNIVERSE: Uuid = Uuid::from_u128(11);
    const DAY_MS: i64 = 1_756_512_000_000; // 2025-08-30T00:00:00Z
    const NOW: i64 = DAY_MS + 10 * 60 * 60 * 1000;

    fn config() -> BotControllerConfig {
        BotControllerConfig {
            universe_id: UNIVERSE,
            bot_id: BotId::new("triage"),
            display_name: None,
            profile_id: ProfileId::new("triager"),
            brief: None,
            runs_per_day: None,
            routed_session_close_after_ms: None,
            self_config: false,
            emit: false,
            enabled: true,
            closed: false,
        }
    }

    fn fresh() -> ControllerState {
        ControllerState::new(
            BotControllerArgs {
                config: config(),
                carry: None,
            },
            NOW,
        )
    }

    fn ready(mut state: ControllerState) -> ControllerState {
        state.mark_session_ready(3);
        state
    }

    fn event(id: &str, seq: u64) -> BotEvent {
        BotEvent {
            id: id.to_owned(),
            seq,
            document_ref: "sha256:doc".to_owned(),
            prompt_ref: None,
            session: None,
            coalesce: None,
            when_busy: None,
            hops: 0,
            reply: false,
            media: Vec::new(),
            tools_ref: None,
            notify: false,
        }
    }

    fn routed(id: &str, seq: u64, session: &str) -> BotEvent {
        let mut event = event(id, seq);
        event.session = Some(RoutedSession {
            session_id: session.to_owned(),
            label: "pr 42".to_owned(),
            session_key: None,
            close_policy: RoutedSessionClosePolicy::Inherit,
        });
        event
    }

    fn coalesced(id: &str, seq: u64, key: &str) -> BotEvent {
        let mut event = event(id, seq);
        event.coalesce = Some(BotCoalesceParams {
            key: key.to_owned(),
            debounce_ms: 400,
            max_wait_ms: 1_500,
            max_count: 3,
        });
        event
    }

    fn invocation_ref(run_id: &str, tool_id: &str, args: &str) -> BotToolInvocationRef {
        BotToolInvocationRef {
            invocation_id: format!("wti:{tool_id}:{args}"),
            tool_id: tool_id.to_owned(),
            run_id: run_id.to_owned(),
            arguments_ref: args.to_owned(),
        }
    }

    fn terminal(status: RunStatus) -> LaneTerminal {
        LaneTerminal {
            status,
            run_id: "run_7".to_owned(),
            failure_message_ref: None,
        }
    }

    fn run_terminal_envelope(session: &str, run: u64, token: &str) -> EmissionEnvelope {
        EmissionEnvelope::run_terminal(
            UNIVERSE,
            SessionId::new(session),
            EventSeq::new(4),
            token.to_owned(),
            RunId::new(run),
            RunStatus::Completed,
            None,
            None,
        )
    }

    fn invocation(session: &str, tool_id: &str, suffix: char) -> WorkflowToolInvocation {
        let mut promises = BTreeMap::new();
        promises.insert(
            harness::REPLY_COMPLETION_KEY.to_owned(),
            PromiseId::new("promise_3"),
        );
        WorkflowToolInvocation {
            invocation_id: harness::WorkflowToolInvocationId::new(format!(
                "wti:sha256:{}",
                suffix.to_string().repeat(64)
            )),
            tool_id: harness::WorkflowToolId::new(tool_id),
            semantic_type: "lightspeed.bots.tool".to_owned(),
            schema_revision: 1,
            binding_fingerprint: "fp".to_owned(),
            session_universe_id: UNIVERSE,
            session_id: SessionId::new(session),
            run_id: RunId::new(7),
            turn_id: harness::TurnId::new(1),
            tool_batch_id: harness::ToolBatchId::new(1),
            tool_call_id: harness::ToolCallId::new("call_1"),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            execution_context_ref: None,
            completion_promises: Some(promises),
        }
    }

    // ── Event acceptance ────────────────────────────────────────────────

    #[test]
    fn duplicate_event_ids_are_counted_not_queued() {
        let mut state = fresh();
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.accept_event(event("e2", 2), NOW).unwrap();
        assert_eq!(state.pending_deliveries.len(), 2);
        assert_eq!(state.duplicate_events, 1);
        assert_eq!(state.event_tick, 3, "every signal wakes the loop");
        assert_eq!(state.pending_deliveries[0].id, "e1");
        assert_eq!(state.pending_deliveries[0].when_busy, BotWhenBusy::Queue);
    }

    #[test]
    fn malformed_events_are_refused() {
        let mut state = fresh();
        let mut bad = event("", 1);
        assert!(state.accept_event(bad.clone(), NOW).is_err());
        bad.id = "e1".to_owned();
        bad.coalesce = Some(BotCoalesceParams {
            key: "k".to_owned(),
            debounce_ms: 500,
            max_wait_ms: 100,
            max_count: 1,
        });
        assert!(state.accept_event(bad, NOW).is_err());
        assert!(state.pending_deliveries.is_empty());
        assert!(
            state.seen_event_ids.is_empty(),
            "a refused event is not seen"
        );
    }

    #[test]
    fn coalesced_events_share_a_buffer_and_flush_on_debounce_or_max_wait() {
        let mut state = fresh();
        state
            .accept_event(coalesced("e1", 1, "gh|main"), NOW)
            .unwrap();
        state
            .accept_event(coalesced("e2", 2, "gh|main"), NOW + 300)
            .unwrap();
        assert_eq!(state.buffers.len(), 1);
        assert!(state.pending_deliveries.is_empty());
        let buffer = &state.buffers["gh|main"];
        assert_eq!(buffer.first_at_ms, NOW);
        assert_eq!(buffer.last_at_ms, NOW + 300);
        assert_eq!(
            buffer.flush_at_ms(),
            NOW + 700,
            "debounce after the last event"
        );
        assert_eq!(state.next_buffer_deadline(), Some(NOW + 700));

        state.flush_ripe_buffers(NOW + 699);
        assert_eq!(state.buffers.len(), 1, "not ripe yet");
        state.flush_ripe_buffers(NOW + 700);
        assert!(state.buffers.is_empty());
        assert_eq!(state.pending_deliveries.len(), 1);
        let delivery = &state.pending_deliveries[0];
        assert_eq!(delivery.events.len(), 2);
        assert_eq!(
            delivery.id,
            ids::delivery_id(&["e1".to_owned(), "e2".to_owned()])
        );
        assert_eq!(delivery.seqs(), vec![1, 2]);

        // Max wait bounds a stream of events that keeps resetting the debounce.
        let mut state = fresh();
        for (index, at) in [0, 300, 600, 900, 1_200].into_iter().enumerate() {
            let mut event = coalesced(&format!("s{index}"), index as u64 + 1, "gh|main");
            event.coalesce.as_mut().unwrap().max_count = 100;
            state.accept_event(event, NOW + at).unwrap();
        }
        assert_eq!(state.buffers["gh|main"].flush_at_ms(), NOW + 1_500);
        state.flush_ripe_buffers(NOW + 1_500);
        assert_eq!(state.pending_deliveries.len(), 1);
        assert_eq!(state.pending_deliveries[0].events.len(), 5);
    }

    #[test]
    fn coalesce_max_count_flushes_at_once() {
        let mut state = fresh();
        state.accept_event(coalesced("e1", 1, "k"), NOW).unwrap();
        state.accept_event(coalesced("e2", 2, "k"), NOW).unwrap();
        assert!(state.pending_deliveries.is_empty());
        state.accept_event(coalesced("e3", 3, "k"), NOW).unwrap();
        assert!(state.buffers.is_empty());
        assert_eq!(state.pending_deliveries.len(), 1);
        assert_eq!(state.pending_deliveries[0].events.len(), 3);
    }

    // ── Config and rotation signals ─────────────────────────────────────

    #[test]
    fn config_identity_changes_are_rejected_and_renames_marked() {
        let mut state = ready(fresh());
        let mut other = config();
        other.bot_id = BotId::new("other");
        assert!(state.accept_config(other).is_err());
        assert!(!state.config_dirty);

        let mut renamed = config();
        renamed.display_name = Some("Triage".to_owned());
        state.accept_config(renamed).unwrap();
        assert!(state.config_dirty);
        assert!(state.rename_dirty);
        assert_eq!(state.bot_label(), "bot Triage");
        assert_eq!(state.routed_label("pr 42"), "bot Triage · pr 42");
    }

    #[test]
    fn rotation_requests_park_dispatch_for_that_target() {
        let mut state = ready(fresh());
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.request_rotation(state.main_session_id());
        assert!(!state.dispatchable(NOW));
        assert!(state.dispatch(NOW).is_empty());
        assert_eq!(state.pending_deliveries.len(), 1);
        state.request_rotation(String::new());
        assert_eq!(state.rotation_requests.len(), 1, "empty ids are ignored");
    }

    // ── Budget ──────────────────────────────────────────────────────────

    #[test]
    fn budget_counts_runs_descendants_and_pending_starts() {
        let mut state = ready(fresh());
        state.config.runs_per_day = Some(3);
        state.runs_today = 1;
        state.descendants_today = 1;
        assert_eq!(state.reserved_runs(), 2);
        assert!(!state.budget_exhausted_view(NOW));

        let mut append = event("e1", 1);
        append.when_busy = Some(BotWhenBusy::Append);
        append.session = Some(RoutedSession {
            session_id: "bot:v1:triage:k-a-1".to_owned(),
            label: "a".to_owned(),
            session_key: None,
            close_policy: RoutedSessionClosePolicy::Inherit,
        });
        state.accept_event(append, NOW).unwrap();
        state.accept_event(event("e2", 2), NOW).unwrap();
        state.accept_event(event("e3", 3), NOW).unwrap();
        let actions = state.dispatch(NOW);
        assert_eq!(actions.len(), 2, "the append lane does not reserve a run");
        assert!(
            matches!(&actions[0], DispatchAction::Lane { delivery, .. } if delivery.id == "e1")
        );
        assert!(
            matches!(&actions[1], DispatchAction::Lane { delivery, .. } if delivery.id == "e2")
        );
        assert_eq!(
            state.reserved_runs(),
            3,
            "the queue lane reserved the last run"
        );
        assert!(state.budget_exhausted_view(NOW));
        assert_eq!(
            state.controller_status(NOW),
            BotControllerStatus::DeliveringEvent
        );
        assert_eq!(state.pending_deliveries.len(), 1, "e3 waits for tomorrow");

        state.release_lane("bot:v1:triage:k-a-1", NOW);
        state.release_lane("bot:v1:triage", NOW);
        state.runs_today = 2;
        assert!(state.dispatch(NOW).is_empty(), "no run left today");
        assert!(!state.dispatchable(NOW));
        let deadline = state
            .wake_deadline(NOW)
            .expect("budget parks until midnight");
        assert_eq!(deadline, DAY_MS + MS_PER_DAY);

        // The pure view never rolls the day; the loop's check does.
        let tomorrow = DAY_MS + MS_PER_DAY + 1;
        assert!(!state.budget_exhausted_view(tomorrow));
        assert_eq!(state.runs_today, 2);
        assert!(!state.budget_exhausted(tomorrow));
        assert_eq!(state.runs_today, 0);
        assert_eq!(state.descendants_today, 0);
        assert_eq!(state.run_day, "2025-08-31");
        assert!(state.budget_roots.is_empty());
    }

    #[test]
    fn budget_exhausted_status_needs_pending_work() {
        let mut state = ready(fresh());
        state.config.runs_per_day = Some(1);
        state.runs_today = 1;
        assert_eq!(state.controller_status(NOW), BotControllerStatus::Idle);
        state.accept_event(event("e1", 1), NOW).unwrap();
        assert_eq!(
            state.controller_status(NOW),
            BotControllerStatus::BudgetExhausted
        );
    }

    #[test]
    fn descendant_refresh_is_due_after_a_delivery_and_periodically_while_busy() {
        let mut state = ready(fresh());
        assert!(!state.descendants_refresh_due(NOW), "no budget, no refresh");
        state.config.runs_per_day = Some(5);
        assert!(state.descendants_refresh_due(NOW), "first refresh");
        state.descendants_refreshed_at_ms = NOW;
        state.descendants_refreshed_processed = Some(0);
        assert!(!state.descendants_refresh_due(NOW + 1_000));
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.dispatch(NOW);
        assert!(!state.descendants_refresh_due(NOW + 59_000));
        assert!(state.descendants_refresh_due(NOW + 60_000));
        assert_eq!(state.wake_deadline(NOW), Some(NOW + 60_000));
        state.events_processed = 1;
        assert!(state.descendants_refresh_due(NOW + 1));
        let roots = state.budget_root_ids();
        assert_eq!(roots, vec!["bot:v1:triage".to_owned()]);
        assert_eq!(state.day_start_ms(), DAY_MS);
    }

    // ── Dispatch ────────────────────────────────────────────────────────

    #[test]
    fn dispatch_takes_free_targets_and_sidecars_busy_ones() {
        let mut state = ready(fresh());
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.accept_event(event("e2", 2), NOW).unwrap();
        let actions = state.dispatch(NOW);
        assert_eq!(actions.len(), 1, "one lane per session");
        assert!(
            matches!(&actions[0], DispatchAction::Lane { target, .. } if target == "bot:v1:triage")
        );
        assert_eq!(state.pending_deliveries.len(), 1);
        assert!(!state.dispatchable(NOW), "queue waits for the lane");

        // A started run admits one steer sidecar; queue stays queued.
        let mut steer = event("e3", 3);
        steer.when_busy = Some(BotWhenBusy::Steer);
        state.accept_event(steer, NOW).unwrap();
        assert!(!state.dispatchable(NOW), "no run started yet");
        state.mark_lane_started("bot:v1:triage", "run_1", NOW);
        assert!(state.dispatchable(NOW));
        let actions = state.dispatch(NOW);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], DispatchAction::Sidecar { delivery, .. } if delivery.id == "e3")
        );
        assert_eq!(state.pending_deliveries.len(), 1, "e2 (queue) stays");
        assert_eq!(state.pending_deliveries[0].id, "e2");

        let mut another = event("e4", 4);
        another.when_busy = Some(BotWhenBusy::Append);
        state.accept_event(another, NOW).unwrap();
        assert!(state.dispatch(NOW).is_empty(), "one sidecar per session");
        state.release_sidecar("bot:v1:triage", NOW);
        let actions = state.dispatch(NOW);
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(&actions[0], DispatchAction::Sidecar { delivery, .. } if delivery.id == "e4")
        );
    }

    #[test]
    fn dispatch_resolves_routed_targets_through_generations() {
        let mut state = ready(fresh());
        state
            .accept_event(routed("e1", 1, "bot:v1:triage:k-pr-42-abcdef01"), NOW)
            .unwrap();
        state
            .accept_event(routed("e2", 2, "bot:v1:triage:k-pr-43-abcdef02"), NOW)
            .unwrap();
        state
            .session_generations
            .insert("bot:v1:triage:k-pr-43-abcdef02".to_owned(), 3);
        let actions = state.dispatch(NOW);
        assert_eq!(actions.len(), 2, "distinct sessions run concurrently");
        assert!(
            matches!(&actions[0], DispatchAction::Lane { target, .. } if target == "bot:v1:triage:k-pr-42-abcdef01")
        );
        assert!(
            matches!(&actions[1], DispatchAction::Lane { target, .. } if target == "bot:v1:triage:k-pr-43-abcdef02-g3")
        );
        assert!(
            state
                .active_by_session
                .contains_key("bot:v1:triage:k-pr-43-abcdef02-g3")
        );
    }

    #[test]
    fn dispatch_preconditions_hold_the_queue() {
        let mut state = ready(fresh());
        state.accept_event(event("e1", 1), NOW).unwrap();
        assert!(state.dispatchable(NOW));
        state.config.enabled = false;
        assert!(!state.dispatchable(NOW));
        state.config.enabled = true;
        state.config_dirty = true;
        assert!(!state.dispatchable(NOW));
        state.config_dirty = false;
        state.session_ready = false;
        assert!(!state.dispatchable(NOW));
        state.session_ready = true;
        state.config.closed = true;
        assert!(!state.dispatchable(NOW));
    }

    #[test]
    fn requeue_front_and_rekey_keep_lane_bookkeeping_consistent() {
        let mut state = ready(fresh());
        state
            .accept_event(routed("e1", 1, "bot:v1:triage:k-x-00000000"), NOW)
            .unwrap();
        state.dispatch(NOW);
        state.rekey_lane(
            "bot:v1:triage:k-x-00000000",
            "bot:v1:triage:k-x-00000000-g2",
        );
        assert!(state.is_session_busy("bot:v1:triage:k-x-00000000-g2"));
        assert!(!state.is_session_busy("bot:v1:triage:k-x-00000000"));
        assert_eq!(
            state.active_by_session["bot:v1:triage:k-x-00000000-g2"].session_id,
            "bot:v1:triage:k-x-00000000-g2"
        );
        let delivery = state.active_by_session["bot:v1:triage:k-x-00000000-g2"]
            .delivery
            .clone();
        state.accept_event(event("e2", 2), NOW).unwrap();
        state.release_lane("bot:v1:triage:k-x-00000000-g2", NOW);
        state.requeue_front(delivery);
        assert_eq!(state.pending_deliveries[0].id, "e1");
        assert_eq!(state.pending_deliveries[1].id, "e2");
    }

    // ── Resolve correlation ─────────────────────────────────────────────

    #[test]
    fn resolve_correlates_by_run_and_the_last_call_wins() {
        let invocations = vec![
            invocation_ref("run_6", BOT_EVENT_RESOLVE_TOOL_ID, "old"),
            invocation_ref("run_7", "lightspeed.bots.event.read.v1", "read"),
            invocation_ref("run_7", BOT_EVENT_RESOLVE_TOOL_ID, "first"),
            invocation_ref("run_7", BOT_EVENT_RESOLVE_TOOL_ID, "last"),
        ];
        let resolves = resolve_invocations_for_run(&invocations, "run_7");
        assert_eq!(
            resolves
                .iter()
                .map(|invocation| invocation.arguments_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "last"]
        );
        assert!(resolve_invocations_for_run(&invocations, "run_8").is_empty());
    }

    #[test]
    fn carried_tool_use_counts_as_handled() {
        let invocations = vec![
            invocation_ref("run_7", "lightspeed.channels.message.send.v1", "a"),
            invocation_ref("run_8", "lightspeed.channels.message.send.v1", "b"),
        ];
        let carried = vec!["lightspeed.channels.message.send.v1".to_owned()];
        assert!(used_carried_tool(&invocations, "run_7", &carried));
        assert!(!used_carried_tool(&invocations, "run_9", &carried));
        assert!(!used_carried_tool(&invocations, "run_7", &[]));
    }

    #[test]
    fn terminals_attach_by_delivery_token() {
        let mut state = ready(fresh());
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.dispatch(NOW);
        let tick = state.lane_tick;
        assert!(
            !state.attach_terminal("bot-event-terminal-v1-nope", terminal(RunStatus::Completed))
        );
        assert!(state.attach_terminal(
            &ids::delivery_terminal_token("e1"),
            terminal(RunStatus::Completed)
        ));
        assert!(state.lane_has_terminal("bot:v1:triage"));
        assert!(state.lane_tick > tick);
    }

    // ── Outcome ladder ──────────────────────────────────────────────────

    #[test]
    fn outcome_ladder() {
        let timed_out = delivery_outcome(None, None, Some("run_7"));
        assert_eq!(timed_out.outcome, BotEventOutcome::RunFailed);
        assert_eq!(
            timed_out.summary.as_deref(),
            Some("timed out waiting for the run terminal")
        );
        assert_eq!(timed_out.run_id.as_deref(), Some("run_7"));

        let failed = delivery_outcome(Some(&terminal(RunStatus::Failed)), None, None);
        assert_eq!(failed.outcome, BotEventOutcome::RunFailed);
        assert_eq!(failed.summary.as_deref(), Some("run ended failed"));
        assert_eq!(
            failed.run_id.as_deref(),
            Some("run_7"),
            "the terminal's run id fills in"
        );

        let resolved = delivery_outcome(
            Some(&terminal(RunStatus::Completed)),
            Some(&LaneResolution {
                outcome: BotEventOutcome::Deferred,
                summary: Some("later".to_owned()),
            }),
            Some("run_7"),
        );
        assert_eq!(resolved.outcome, BotEventOutcome::Deferred);
        assert_eq!(resolved.summary.as_deref(), Some("later"));

        let unresolved =
            delivery_outcome(Some(&terminal(RunStatus::Completed)), None, Some("run_7"));
        assert_eq!(unresolved.outcome, BotEventOutcome::Unresolved);
        assert_eq!(unresolved.summary, None);
    }

    // ── Emissions ───────────────────────────────────────────────────────

    #[test]
    fn emissions_are_deduped_and_classified() {
        let mut state = ready(fresh());
        let token = ids::delivery_terminal_token("e1");
        let envelope = run_terminal_envelope("bot:v1:triage", 7, &token);
        match state.classify_emission(envelope.clone()) {
            Some(EmissionDisposition::Terminal {
                session_id,
                run_id,
                token: seen_token,
                status,
                ..
            }) => {
                assert_eq!(session_id, "bot:v1:triage");
                assert_eq!(run_id, "run_7");
                assert_eq!(seen_token, token);
                assert_eq!(status, RunStatus::Completed);
            }
            other => panic!("unexpected disposition {other:?}"),
        }
        assert_eq!(state.classify_emission(envelope), None, "duplicate");
        assert_eq!(state.duplicate_emissions, 1);

        let foreign = run_terminal_envelope("bot:v1:other", 7, &token);
        assert!(matches!(
            state.classify_emission(foreign),
            Some(EmissionDisposition::Foreign(_))
        ));
        let mut cross_universe = run_terminal_envelope("bot:v1:triage:k-a-1", 8, &token);
        cross_universe.producer = EmissionProducer::Session {
            universe_id: Uuid::from_u128(99),
            session_id: SessionId::new("bot:v1:triage:k-a-1"),
            log_seq: EventSeq::new(1),
        };
        assert!(matches!(
            state.classify_emission(cross_universe),
            Some(EmissionDisposition::Foreign(_))
        ));

        let resolution = EmissionEnvelope::source_resolution(
            UNIVERSE,
            "other-workflow".to_owned(),
            "holder",
            PromiseId::new("promise_1"),
            PromiseResolution::Resolved { payload_ref: None },
        );
        assert_eq!(
            state.classify_emission(resolution),
            Some(EmissionDisposition::Ignore)
        );
    }

    #[test]
    fn pushed_invocations_are_handled_once_and_pull_tools_ignored() {
        let mut state = ready(fresh());
        let pushed = EmissionEnvelope::tool_invocation(
            UNIVERSE,
            SessionId::new("bot:v1:triage"),
            EventSeq::new(3),
            invocation("bot:v1:triage", "lightspeed.bots.status.v1", 'a'),
            "holder-workflow".to_owned(),
        );
        assert!(matches!(
            state.classify_emission(pushed.clone()),
            Some(EmissionDisposition::Invocation { ref session_id, ref holder_workflow_id, .. })
                if session_id == "bot:v1:triage" && holder_workflow_id == "holder-workflow"
        ));
        // A second push of the same invocation under a fresh emission id
        // is still the same invocation.
        let mut again = pushed.clone();
        again.emission_id = harness::EmissionId::for_source_resolution(
            UNIVERSE,
            "x",
            "y",
            &PromiseId::new("promise_9"),
        );
        assert_eq!(
            state.classify_emission(again),
            Some(EmissionDisposition::Ignore)
        );

        let pull = EmissionEnvelope::tool_invocation(
            UNIVERSE,
            SessionId::new("bot:v1:triage"),
            EventSeq::new(5),
            invocation("bot:v1:triage", BOT_EVENT_RESOLVE_TOOL_ID, 'b'),
            "holder-workflow".to_owned(),
        );
        assert_eq!(
            state.classify_emission(pull),
            Some(EmissionDisposition::Ignore),
            "bot_event_resolve is pulled from the log, never pushed"
        );
    }

    #[test]
    fn controller_summary_carries_hops_and_the_routed_base() {
        let mut state = ready(fresh());
        let mut event = routed("e1", 1, "bot:v1:triage:k-pr-42-abcdef01");
        event.hops = 3;
        state.accept_event(event, NOW).unwrap();
        state
            .session_generations
            .insert("bot:v1:triage:k-pr-42-abcdef01".to_owned(), 2);
        state.dispatch(NOW);
        state.extra_sessions.push(ManagedSession {
            session_id: "bot:v1:triage:k-pr-42-abcdef01-g2".to_owned(),
            label: "pr 42".to_owned(),
            session_key: Some("pr-42".to_owned()),
            kind: BotSessionKind::PerKey,
            last_active_at_ms: Some(NOW),
            close_policy: RoutedSessionClosePolicy::Never,
            carried_tool_ids: Vec::new(),
        });
        let summary = state.controller_summary("bot:v1:triage:k-pr-42-abcdef01-g2", NOW);
        assert_eq!(summary.hops, 3);
        let routed = summary.routed_session.expect("routed session");
        assert_eq!(routed.session_id, "bot:v1:triage:k-pr-42-abcdef01");
        assert_eq!(routed.label, "pr 42");
        assert_eq!(routed.session_key.as_deref(), Some("pr-42"));
        assert_eq!(summary.snapshot.active_deliveries.len(), 1);
        let main = state.controller_summary("bot:v1:triage", NOW);
        assert_eq!(main.hops, 0);
        assert!(main.routed_session.is_none());
    }

    // ── Retention ───────────────────────────────────────────────────────

    fn managed(
        id: &str,
        close_policy: RoutedSessionClosePolicy,
        last_active: i64,
    ) -> ManagedSession {
        ManagedSession {
            session_id: id.to_owned(),
            label: id.to_owned(),
            session_key: None,
            kind: BotSessionKind::PerKey,
            last_active_at_ms: Some(last_active),
            close_policy,
            carried_tool_ids: Vec::new(),
        }
    }

    #[test]
    fn idle_close_uses_the_session_policy_then_the_bot_default() {
        let mut state = ready(fresh());
        state.config.routed_session_close_after_ms = Some(10_000);
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-a-1",
            RoutedSessionClosePolicy::Inherit,
            NOW,
        ));
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-b-1",
            RoutedSessionClosePolicy::Never,
            NOW,
        ));
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-c-1",
            RoutedSessionClosePolicy::After { ms: 2_000 },
            NOW,
        ));
        assert_eq!(state.next_idle_close_deadline(), Some(NOW + 2_000));
        assert!(state.expired_sessions(NOW + 1_999).is_empty());
        assert_eq!(
            state.expired_sessions(NOW + 2_000),
            vec!["bot:v1:triage:k-c-1".to_owned()]
        );
        assert_eq!(
            state.expired_sessions(NOW + 10_000),
            vec![
                "bot:v1:triage:k-a-1".to_owned(),
                "bot:v1:triage:k-c-1".to_owned()
            ]
        );
        state.config.routed_session_close_after_ms = None;
        assert_eq!(state.next_idle_close_deadline(), Some(NOW + 2_000));
        assert_eq!(
            state.expired_sessions(NOW + 10_000),
            vec!["bot:v1:triage:k-c-1".to_owned()],
            "absent bot idle-close policy keeps inherited sessions"
        );

        // Busy sessions never expire; forgetting bumps the generation.
        state
            .accept_event(routed("e1", 1, "bot:v1:triage:k-c-1"), NOW)
            .unwrap();
        state.dispatch(NOW);
        assert!(state.expired_sessions(NOW + 10_000).is_empty());
        state.release_lane("bot:v1:triage:k-c-1", NOW + 20_000);
        assert_eq!(
            state.next_idle_close_deadline(),
            Some(NOW + 22_000),
            "touched"
        );
        state.forget_routed_session("bot:v1:triage:k-c-1");
        assert_eq!(state.extra_sessions.len(), 2);
        assert_eq!(
            state.resolve_routed_session_id("bot:v1:triage:k-c-1"),
            "bot:v1:triage:k-c-1-g2"
        );
    }

    #[test]
    fn routed_events_refresh_idle_close_and_lru_activity() {
        let mut state = ready(fresh());
        state.config.routed_session_close_after_ms = Some(10_000);
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-a-1",
            RoutedSessionClosePolicy::Inherit,
            NOW,
        ));
        let routed = RoutedSession {
            session_id: "bot:v1:triage:k-a-1".to_owned(),
            label: "a".to_owned(),
            session_key: None,
            close_policy: RoutedSessionClosePolicy::Inherit,
        };

        assert!(state.observe_routed_session(&routed.session_id, &routed, NOW + 5_000));
        assert!(state.expired_sessions(NOW + 10_000).is_empty());
        assert_eq!(state.next_idle_close_deadline(), Some(NOW + 15_000));
    }

    #[test]
    fn routed_keys_survive_session_rotation_and_legacy_activity_payloads_load() {
        let mut state = ready(fresh());
        let route = RoutedSession {
            session_id: ids::bot_keyed_session_id(&state.config.bot_id, "telegram:12345"),
            label: "Ops room".to_owned(),
            session_key: Some("telegram:12345".to_owned()),
            close_policy: RoutedSessionClosePolicy::Never,
        };
        for generation in 1..=2 {
            let session_id = state.resolve_routed_session_id(&route.session_id);
            assert_eq!(
                session_id,
                ids::routed_session_generation_id(&route.session_id, generation)
            );
            let request = super::super::ensure_request(
                &state,
                "controller",
                session_id.clone(),
                state.routed_label(&route.label),
                None,
                None,
                Some(&route),
            );
            assert_eq!(request.session_id, session_id);
            assert_eq!(request.session_key.as_deref(), Some("telegram:12345"));
            assert_eq!(request.session_label.as_deref(), Some("Ops room"));
            let mut wire = serde_json::to_value(&request).unwrap();
            assert_eq!(
                serde_json::from_value::<super::super::BotEnsureSessionRequest>(wire.clone())
                    .unwrap(),
                request
            );
            wire.as_object_mut().unwrap().remove("session_key");
            wire.as_object_mut().unwrap().remove("session_label");
            let legacy: super::super::BotEnsureSessionRequest =
                serde_json::from_value(wire).unwrap();
            assert_eq!(legacy.session_id, session_id);
            assert_eq!(legacy.session_key, None);
            assert_eq!(legacy.session_label, None);
            state.bump_routed_generation(&route.session_id);
        }
        let main = super::super::ensure_request(
            &state,
            "controller",
            state.main_session_id(),
            state.bot_label(),
            None,
            None,
            None,
        );
        assert_eq!(main.session_key, None);
        assert_eq!(main.session_label, None);
    }

    #[test]
    fn observing_a_new_event_recovers_a_known_threads_key_without_erasing_it() {
        let mut state = ready(fresh());
        let id = ids::bot_keyed_session_id(&state.config.bot_id, "telegram:12345");
        state
            .extra_sessions
            .push(managed(&id, RoutedSessionClosePolicy::Never, NOW));
        let mut route = RoutedSession {
            session_id: id.clone(),
            label: "Ops room".to_owned(),
            session_key: Some("telegram:12345".to_owned()),
            close_policy: RoutedSessionClosePolicy::Never,
        };
        assert!(state.observe_routed_session(&id, &route, NOW + 1));
        route.session_key = None;
        assert!(state.observe_routed_session(&id, &route, NOW + 2));
        let summary = state.controller_summary(&id, NOW + 2);
        assert_eq!(
            summary.routed_session.unwrap().session_key.as_deref(),
            Some("telegram:12345")
        );
    }

    #[test]
    fn idlest_free_session_is_the_eviction_candidate() {
        let mut state = ready(fresh());
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-a-1",
            RoutedSessionClosePolicy::Inherit,
            NOW + 5,
        ));
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-b-1",
            RoutedSessionClosePolicy::Inherit,
            NOW + 1,
        ));
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-c-1",
            RoutedSessionClosePolicy::Inherit,
            NOW + 3,
        ));
        state
            .sidecar_by_session
            .insert("bot:v1:triage:k-b-1".to_owned());
        assert_eq!(
            state.idlest_free_session("bot:v1:triage:k-z-1"),
            Some("bot:v1:triage:k-c-1".to_owned())
        );
        assert_eq!(
            state.idlest_free_session("bot:v1:triage:k-c-1"),
            Some("bot:v1:triage:k-a-1".to_owned())
        );
    }

    // ── Main session lifecycle ──────────────────────────────────────────

    #[test]
    fn main_rotation_and_readiness() {
        let mut state = fresh();
        assert_eq!(state.main_session_id(), "bot:v1:triage");
        assert_eq!(state.setup_status, BotSetupStatus::Initializing);
        assert!(state.config_dirty);
        state.mark_session_ready(4);
        assert!(state.session_ready);
        assert_eq!(state.applied_revision_for_ensure(), Some(4));
        assert_eq!(state.tools_revision, Some(BOT_TOOLS_REVISION));

        let mut other_profile = config();
        other_profile.profile_id = ProfileId::new("elsewhere");
        state.accept_config(other_profile).unwrap();
        assert_eq!(
            state.applied_revision_for_ensure(),
            None,
            "another profile applies fresh"
        );

        state.rotate_main_session(false);
        assert_eq!(state.main_session_id(), "bot:v1:triage-g2");
        assert!(
            state.session_ready,
            "a mismatch rotation keeps readiness until reconcile"
        );
        state.rotate_main_session(true);
        assert_eq!(state.main_session_id(), "bot:v1:triage-g3");
        assert!(!state.session_ready);
        assert_eq!(state.setup_status, BotSetupStatus::Initializing);

        state.mark_session_degraded("boom".to_owned());
        assert_eq!(state.setup_status, BotSetupStatus::Degraded);
        assert_eq!(state.controller_status(NOW), BotControllerStatus::Degraded);
        assert!(!state.config_dirty);
    }

    // ── Teardown ────────────────────────────────────────────────────────

    #[test]
    fn teardown_sets_union_pending_buffered_and_active_events() {
        let mut state = ready(fresh());
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.accept_event(event("e2", 2), NOW).unwrap();
        state.accept_event(coalesced("e3", 3, "k"), NOW).unwrap();
        state.dispatch(NOW);
        state.rotate_main_session(true);
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-a-1",
            RoutedSessionClosePolicy::Inherit,
            NOW,
        ));
        assert_eq!(
            state.teardown_event_ids(),
            vec!["e2".to_owned(), "e3".to_owned(), "e1".to_owned()]
        );
        assert_eq!(
            state.teardown_sessions(),
            vec![
                "bot:v1:triage".to_owned(),
                "bot:v1:triage-g2".to_owned(),
                "bot:v1:triage:k-a-1".to_owned()
            ]
        );
        state.closing = true;
        assert_eq!(state.controller_status(NOW), BotControllerStatus::Closing);
        state.closed_done = true;
        assert_eq!(state.controller_status(NOW), BotControllerStatus::Closed);
    }

    // ── Loop control and carry ──────────────────────────────────────────

    #[test]
    fn wake_and_continue_as_new_conditions() {
        let mut state = ready(fresh());
        let (tick, event_tick) = (state.lane_tick, state.event_tick);
        assert!(!state.wake_ready(tick, event_tick));
        state.accept_event(event("e1", 1), NOW).unwrap();
        assert!(
            state.wake_ready(tick, event_tick),
            "an accepted event wakes the loop"
        );
        let event_tick = state.event_tick;
        state.dispatch(NOW);
        assert!(!state.wake_ready(state.lane_tick, event_tick));
        state.queue_emission(run_terminal_envelope("bot:v1:triage", 1, "t"));
        assert!(state.wake_ready(state.lane_tick, event_tick));
        state.emission_inbox.clear();
        state.config_dirty = true;
        assert!(
            !state.wake_ready(state.lane_tick, event_tick),
            "main is busy"
        );
        state.release_lane("bot:v1:triage", NOW);
        assert!(state.wake_ready(state.lane_tick, event_tick));

        state.config_dirty = false;
        assert!(!state.can_continue_as_new(false, false));
        state.events_processed = BOT_CONTINUE_AS_NEW_AFTER_EVENTS;
        assert!(state.can_continue_as_new(false, false));
        assert!(
            !state.can_continue_as_new(false, true),
            "lanes still running"
        );
        state.events_processed = 1;
        assert!(state.can_continue_as_new(true, false), "server suggestion");
        state.config.closed = true;
        assert!(
            !state.can_continue_as_new(true, false),
            "closing never continues as new"
        );
    }

    #[test]
    fn carry_round_trips_and_old_carries_load() {
        let mut state = ready(fresh());
        state.accept_event(event("e1", 1), NOW).unwrap();
        state.accept_event(coalesced("e2", 2, "k"), NOW).unwrap();
        state
            .session_generations
            .insert("bot:v1:triage:k-a-1".to_owned(), 2);
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-a-1-g2",
            RoutedSessionClosePolicy::Never,
            NOW,
        ));
        state.request_rotation("bot:v1:triage:k-a-1-g2".to_owned());
        state.extra_sessions[0].session_key = Some("original routing key".to_owned());
        state.runs_today = 4;
        state.events_processed = 120;
        state.set_cursor("bot:v1:triage", 17);
        state.remember_delivery(BotRecentDeliverySnapshot {
            delivery_id: "e0".to_owned(),
            seqs: vec![0],
            session_id: "bot:v1:triage".to_owned(),
            run_id: Some("run_1".to_owned()),
            outcome: BotEventOutcome::Handled,
            summary: None,
            finished_at_ms: NOW,
            usage: None,
        });
        state.rotate_main_session(false);

        let carry = state.carry();
        let json = serde_json::to_string(&carry).unwrap();
        let decoded: BotControllerCarry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, carry);
        let mut legacy = serde_json::to_value(&carry).unwrap();
        legacy["extra_sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("session_key");
        let legacy: BotControllerCarry = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.extra_sessions[0].session_key, None);
        let next = ControllerState::new(
            BotControllerArgs {
                config: state.config.clone(),
                carry: Some(decoded),
            },
            NOW + 1,
        );
        assert_eq!(next.pending_deliveries, state.pending_deliveries);
        assert_eq!(next.buffers, state.buffers);
        assert_eq!(next.main_session_id(), "bot:v1:triage-g2");
        assert_eq!(next.session_generations, state.session_generations);
        assert_eq!(next.extra_sessions, state.extra_sessions);
        assert_eq!(next.rotation_requests, state.rotation_requests);
        assert_eq!(next.runs_today, 4);
        assert_eq!(next.run_day, state.run_day);
        assert_eq!(
            next.events_processed, 0,
            "the counter restarts per execution"
        );
        assert_eq!(next.cursor_for("bot:v1:triage"), 17);
        assert_eq!(next.recent_deliveries.len(), 1);
        assert!(next.seen_event_ids.contains("e1"));
        assert!(next.seen_event_ids.contains("e2"));
        assert!(next.session_ready);
        assert!(next.config_dirty, "a fresh execution reconciles first");

        let empty: BotControllerCarry = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, BotControllerCarry::default());
        let legacy = ControllerState::new(
            BotControllerArgs {
                config: config(),
                carry: Some(empty),
            },
            NOW,
        );
        assert_eq!(legacy.main_generation, 1);
        assert_eq!(legacy.run_day, "2025-08-30");
    }

    #[test]
    fn seen_ids_are_tail_capped_in_insertion_order() {
        let mut seen = SeenIds::with_cap(3);
        for id in ["a", "b", "c", "d"] {
            assert!(seen.insert(id.to_owned()));
        }
        assert!(!seen.insert("d".to_owned()));
        assert_eq!(seen.tail(), vec!["b", "c", "d"]);
        assert!(!seen.contains("a"));
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn snapshot_reports_sessions_buffers_and_status() {
        let mut state = ready(fresh());
        state.accept_event(coalesced("e1", 1, "k"), NOW).unwrap();
        state.extra_sessions.push(managed(
            "bot:v1:triage:k-a-1",
            RoutedSessionClosePolicy::Inherit,
            NOW,
        ));
        let snapshot = state.snapshot(NOW);
        assert_eq!(snapshot.controller_status, BotControllerStatus::Idle);
        assert_eq!(snapshot.setup_status, BotSetupStatus::Ready);
        assert_eq!(snapshot.main_session_id, "bot:v1:triage");
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].kind, BotSessionKind::Main);
        assert_eq!(snapshot.sessions[1].generation, 1);
        assert_eq!(snapshot.buffers.len(), 1);
        assert_eq!(snapshot.buffers[0].seqs, vec![1]);
        assert_eq!(snapshot.buffers[0].flush_at_ms, NOW + 400);
        assert_eq!(snapshot.pending_deliveries, 0);
        assert_eq!(snapshot.run_day.as_deref(), Some("2025-08-30"));
        assert_eq!(snapshot.applied_profile_revision, Some(3));
    }

    #[test]
    fn utc_day_arithmetic() {
        assert_eq!(utc_day(NOW), "2025-08-30");
        assert_eq!(day_start_ms("2025-08-30"), DAY_MS);
        assert_eq!(ms_until_next_utc_day(NOW), DAY_MS + MS_PER_DAY - NOW);
        assert_eq!(
            ms_until_next_utc_day(DAY_MS + MS_PER_DAY - 1),
            1_000,
            "at least a second"
        );
        assert_eq!(day_start_ms("garbage"), 0);
    }
}
