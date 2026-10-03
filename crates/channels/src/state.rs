//! Conversation workflow state: what the workflow remembers between
//! signals, how it deduplicates, and what continue-as-new carries.
//!
//! Everything here is pure bookkeeping over values the workflow already
//! holds; effects (emitting events, delivering, resolving promises) are the
//! workflow's, driven by the returned effects.

use std::collections::{BTreeMap, HashSet, VecDeque};

use api::{
    BotEventOutcome, ChannelConversationSnapshot, ChatAccess, ChatActivation, ChatGroupActivation,
    ChatScope,
};
use bots::signal::{BotDeliveryPhase, BotDeliveryReceipt};
use serde::{Deserialize, Serialize};

use crate::ChannelError;
use crate::inbound::{ConversationStart, NormalizedInbound};
use crate::policy::initial_group_activation;

/// Message handles kept in workflow state; older numbers still resolve
/// through the bot's event log.
pub const MAX_CHANNEL_HANDLES: usize = 512;
/// Inbound signals buffered in one workflow before shedding.
pub const MAX_CHANNEL_INBOUND_INBOX: usize = 256;
pub const MAX_CARRIED_MESSAGES: usize = 256;
pub const MAX_CARRIED_DELIVERIES: usize = 128;
pub const MAX_CARRIED_INVOCATIONS: usize = 64;
pub const MAX_CARRIED_POLICY_RESPONSES: usize = 64;
pub const MAX_CARRIED_PROTOCOL_ERRORS: usize = 32;
pub const MAX_CARRIED_CANCELLATIONS: usize = 256;
pub const MAX_CARRIED_SEEN_IDS: usize = 2_048;
pub const CONVERSATION_CARRY_VERSION: u32 = 1;

// ── Collections ─────────────────────────────────────────────────────────────

/// A string-keyed map that remembers insertion order (re-inserting a key
/// keeps its position), so "the most recent N entries" is well defined.
/// Serialized as an array of `[key, value]` pairs so the order survives
/// every JSON path, including ones that sort object keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.iter().any(|(candidate, _)| candidate == key)
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries
            .iter_mut()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    /// Insert or replace; the previous value when the key existed.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        let key = key.into();
        match self
            .entries
            .iter_mut()
            .find(|(candidate, _)| *candidate == key)
        {
            Some(slot) => Some(std::mem::replace(&mut slot.1, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        let index = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == key)?;
        Some(self.entries.remove(index).1)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(key, _)| key.as_str())
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// Every entry `keep` selects plus the newest `limit` of the others,
    /// the others first.
    pub fn retain_recent(&self, keep: impl Fn(&V) -> bool, limit: usize) -> Self
    where
        V: Clone,
    {
        let kept = self.entries.iter().filter(|(_, value)| keep(value));
        let rest: Vec<&(String, V)> = self
            .entries
            .iter()
            .filter(|(_, value)| !keep(value))
            .collect();
        let skip = rest.len().saturating_sub(limit);
        Self {
            entries: rest.into_iter().skip(skip).chain(kept).cloned().collect(),
        }
    }
}

/// An insertion-ordered set of ids with a bounded carry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<String>", into = "Vec<String>")]
pub struct SeenSet {
    order: VecDeque<String>,
    index: HashSet<String>,
}

impl SeenSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index.contains(id)
    }

    /// `true` when the id was new.
    pub fn insert(&mut self, id: impl Into<String>) -> bool {
        let id = id.into();
        if !self.index.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    /// The newest `limit` ids, in order.
    pub fn newest(&self, limit: usize) -> Self {
        let skip = self.order.len().saturating_sub(limit);
        self.order.iter().skip(skip).cloned().collect()
    }
}

impl FromIterator<String> for SeenSet {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        let mut set = Self::new();
        for id in iter {
            set.insert(id);
        }
        set
    }
}

impl From<Vec<String>> for SeenSet {
    fn from(ids: Vec<String>) -> Self {
        ids.into_iter().collect()
    }
}

impl From<SeenSet> for Vec<String> {
    fn from(set: SeenSet) -> Self {
        set.order.into()
    }
}

// ── Records ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Emitting,
    Emitted,
    Filtered,
    Duplicate,
    Refused,
    Failed,
}

/// One inbound provider message on its way into the bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedMessage {
    pub message_id: String,
    pub status: MessageStatus,
    /// The bot's event number, once admitted: the handle the model uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The routed session admission chose (logical base id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ReceivedMessage {
    pub fn emitting(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            status: MessageStatus::Emitting,
            seq: None,
            session_id: None,
            error: None,
        }
    }
}

/// Message number → provider ids and direction, both ways.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHandle {
    /// Every provider id the message occupies (a chunked send has
    /// several); the first is the anchor.
    pub provider_message_ids: Vec<String>,
    pub from_me: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Started,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackStatus {
    Reconciling,
    Suppressed,
    Delivered,
    Failed,
}

/// The text-reply fallback of a finished delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryFallback {
    pub status: FallbackStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_message_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DeliveryFallback {
    pub fn status(status: FallbackStatus) -> Self {
        Self {
            status,
            provider_message_ids: Vec::new(),
            seq: None,
            error: None,
        }
    }
}

/// A bot delivery this conversation was told about through delivery receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryRecord {
    pub status: DeliveryStatus,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The lane's finish outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotEventOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<DeliveryFallback>,
}

impl DeliveryRecord {
    /// Still typing, or still deciding on the fallback.
    pub fn is_active(&self) -> bool {
        self.status == DeliveryStatus::Started
            || self
                .fallback
                .as_ref()
                .is_some_and(|fallback| fallback.status == FallbackStatus::Reconciling)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyResponseKind {
    Control,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyResponseStatus {
    Delivering,
    Delivered,
    Failed,
}

/// A Channels-authored reply (control command, denial) sent straight out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyResponse {
    pub kind: PolicyResponseKind,
    pub status: PolicyResponseStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_message_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    Received,
    Delivering,
    Resolved,
    Failed,
    Cancelled,
}

/// A pushed `message_*` invocation and what became of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedInvocation {
    pub invocation_id: String,
    pub tool_id: String,
    /// CAS ref of the model's arguments.
    pub arguments_ref: String,
    /// The session workflow the reply is signalled to.
    pub holder_workflow_id: String,
    pub producer_session_id: String,
    pub status: InvocationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_message_ids: Vec<String>,
    /// The number the send got (`chat.sent` row), when the tool was a send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolution_emission_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ReceivedInvocation {
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            InvocationStatus::Received | InvocationStatus::Delivering
        )
    }
}

// ── Emissions ───────────────────────────────────────────────────────────────

/// The facts of a delivered emission the conversation cares about; the
/// substrate adapter projects the harness envelope onto this.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationEmission {
    pub emission_id: String,
    /// The producing session, when the producer was a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_session_id: Option<String>,
    pub body: EmissionBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum EmissionBody {
    ToolInvocation {
        holder_workflow_id: String,
        invocation_id: String,
        tool_id: String,
        arguments_ref: String,
    },
    RunTerminal,
    InvocationCancellation {
        invocation_id: String,
        completion_key: String,
        promise_id: String,
    },
    SourceResolution,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyEmissionEffect {
    /// A new invocation was recorded as `received`.
    InvocationReceived {
        invocation_id: String,
    },
    /// The session cancelled a keyed promise; stop the work behind it.
    InvocationCancelled {
        invocation_id: String,
        promise_id: String,
    },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptEffect {
    /// Already known in this or a later phase.
    Ignored,
    /// The run is up: show typing.
    Started,
    /// The delivery ended; the workflow decides on the reply fallback.
    Finished,
}

// ── State ───────────────────────────────────────────────────────────────────

/// Everything the conversation workflow remembers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    pub bot_id: String,
    pub trigger_id: String,
    pub label: String,
    pub conversation_key: String,
    pub scope: ChatScope,
    pub activation: ChatActivation,
    pub access: ChatAccess,
    /// The live group mode; `/activation` changes it.
    pub group_activation: ChatGroupActivation,
    /// CAS ref of this conversation's receiver-bound tool declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_ref: Option<String>,
    pub inbound_count: u64,
    pub duplicate_inbound_count: u64,
    pub duplicate_emission_count: u64,
    pub dropped_inbound_count: u64,
    pub overloaded_inbound_count: u64,
    pub denied_inbound_count: u64,
    pub emitted_count: u64,
    pub delivered_count: u64,
    pub failed_delivery_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_errors: Vec<String>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub messages: OrderedMap<ReceivedMessage>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub handles: BTreeMap<u64, ChatHandle>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub deliveries: OrderedMap<DeliveryRecord>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub policy_responses: OrderedMap<PolicyResponse>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub invocations: OrderedMap<ReceivedInvocation>,
    /// `{invocation_id}:{completion_key}` of every cancellation seen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancellations: Vec<String>,
    #[serde(default, skip_serializing_if = "SeenSet::is_empty")]
    pub seen_inbound_ids: SeenSet,
    #[serde(default, skip_serializing_if = "SeenSet::is_empty")]
    pub seen_emission_ids: SeenSet,
}

impl Default for ConversationState {
    fn default() -> Self {
        Self {
            bot_id: String::new(),
            trigger_id: String::new(),
            label: String::new(),
            conversation_key: String::new(),
            scope: ChatScope::Direct,
            activation: ChatActivation::default(),
            access: ChatAccess::default(),
            group_activation: ChatGroupActivation::default(),
            tools_ref: None,
            inbound_count: 0,
            duplicate_inbound_count: 0,
            duplicate_emission_count: 0,
            dropped_inbound_count: 0,
            overloaded_inbound_count: 0,
            denied_inbound_count: 0,
            emitted_count: 0,
            delivered_count: 0,
            failed_delivery_count: 0,
            protocol_errors: Vec::new(),
            messages: OrderedMap::new(),
            handles: BTreeMap::new(),
            deliveries: OrderedMap::new(),
            policy_responses: OrderedMap::new(),
            invocations: OrderedMap::new(),
            cancellations: Vec::new(),
            seen_inbound_ids: SeenSet::new(),
            seen_emission_ids: SeenSet::new(),
        }
    }
}

/// What continue-as-new carries: the compacted state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCarry {
    pub version: u32,
    pub state: ConversationState,
}

/// Dedup key of an inbound message: provider, account, chat, thread, and
/// the provider message id.
pub fn channel_inbound_key(inbound: &NormalizedInbound) -> String {
    [
        inbound.provider.as_str(),
        inbound.account_id.as_str(),
        inbound.inbound.chat_id.as_str(),
        inbound.inbound.thread_id.as_deref().unwrap_or(""),
        inbound.inbound.message_id.as_str(),
    ]
    .join("\u{1f}")
}

impl ConversationState {
    pub fn new(start: &ConversationStart) -> Self {
        Self {
            bot_id: start.bot_id.to_string(),
            trigger_id: start.trigger_id.to_string(),
            label: start.label.clone(),
            conversation_key: start.conversation.key(),
            scope: start.scope,
            activation: start.activation.clone(),
            access: start.access.clone(),
            group_activation: initial_group_activation(&start.activation),
            ..Self::default()
        }
    }

    /// Resume from a carry, which must belong to the same trigger and
    /// conversation.
    pub fn restore(
        start: &ConversationStart,
        carry: ConversationCarry,
    ) -> Result<Self, ChannelError> {
        if carry.version != CONVERSATION_CARRY_VERSION
            || carry.state.trigger_id != start.trigger_id.as_str()
            || carry.state.conversation_key != start.conversation.key()
        {
            return Err(ChannelError::invalid(
                "conversation workflow carry does not match the conversation",
            ));
        }
        Ok(carry.state)
    }

    /// Record an inbound message; its dedup key when new, `None` for a
    /// duplicate.
    pub fn apply_inbound(&mut self, inbound: &NormalizedInbound) -> Option<String> {
        let key = channel_inbound_key(inbound);
        if !self.seen_inbound_ids.insert(key.clone()) {
            self.duplicate_inbound_count += 1;
            return None;
        }
        self.inbound_count += 1;
        Some(key)
    }

    /// Buffer a signal for the workflow loop, shedding beyond the inbox
    /// ceiling; `false` when shed.
    pub fn enqueue_bounded_inbound<T>(&mut self, inbox: &mut Vec<T>, inbound: T) -> bool {
        if inbox.len() >= MAX_CHANNEL_INBOUND_INBOX {
            self.overloaded_inbound_count += 1;
            return false;
        }
        inbox.push(inbound);
        true
    }

    /// Keep the newest handles by number; older numbers still resolve
    /// through the bot's event log.
    pub fn remember_handle(&mut self, seq: u64, handle: ChatHandle) {
        self.handles.insert(seq, handle);
        while self.handles.len() > MAX_CHANNEL_HANDLES {
            self.handles.pop_first();
        }
    }

    pub fn apply_emission(&mut self, emission: &ConversationEmission) -> ApplyEmissionEffect {
        if !self.seen_emission_ids.insert(emission.emission_id.clone()) {
            self.duplicate_emission_count += 1;
            return ApplyEmissionEffect::None;
        }
        match &emission.body {
            EmissionBody::ToolInvocation {
                holder_workflow_id,
                invocation_id,
                tool_id,
                arguments_ref,
            } => {
                let Some(producer_session_id) = &emission.producer_session_id else {
                    self.protocol_errors
                        .push("tool invocation must be produced by a session".to_owned());
                    return ApplyEmissionEffect::None;
                };
                self.invocations.insert(
                    invocation_id.clone(),
                    ReceivedInvocation {
                        invocation_id: invocation_id.clone(),
                        tool_id: tool_id.clone(),
                        arguments_ref: arguments_ref.clone(),
                        holder_workflow_id: holder_workflow_id.clone(),
                        producer_session_id: producer_session_id.clone(),
                        status: InvocationStatus::Received,
                        provider_message_ids: Vec::new(),
                        sent_seq: None,
                        resolution_emission_ids: Vec::new(),
                        error: None,
                    },
                );
                ApplyEmissionEffect::InvocationReceived {
                    invocation_id: invocation_id.clone(),
                }
            }
            // The bot controller is the session's lifecycle controller; a
            // terminal here means a declaration named this workflow where it
            // should not.
            EmissionBody::RunTerminal => {
                self.protocol_errors
                    .push("conversation received a run terminal it does not own".to_owned());
                ApplyEmissionEffect::None
            }
            EmissionBody::InvocationCancellation {
                invocation_id,
                completion_key,
                promise_id,
            } => {
                self.cancellations
                    .push(format!("{invocation_id}:{completion_key}"));
                ApplyEmissionEffect::InvocationCancelled {
                    invocation_id: invocation_id.clone(),
                    promise_id: promise_id.clone(),
                }
            }
            EmissionBody::SourceResolution => {
                self.protocol_errors
                    .push("conversation received an unexpected source resolution".to_owned());
                ApplyEmissionEffect::None
            }
        }
    }

    /// Whether a cancellation for the invocation arrived (possibly before
    /// the invocation itself).
    pub fn is_cancelled(&self, invocation_id: &str) -> bool {
        let prefix = format!("{invocation_id}:");
        self.cancellations
            .iter()
            .any(|fact| fact.starts_with(&prefix))
    }

    pub fn invocation_delivering(&mut self, invocation_id: &str) -> bool {
        self.set_invocation_status(invocation_id, InvocationStatus::Delivering)
    }

    pub fn invocation_resolved(
        &mut self,
        invocation_id: &str,
        provider_message_ids: Vec<String>,
        sent_seq: Option<u64>,
        resolution_emission_ids: Vec<String>,
    ) -> bool {
        let Some(entry) = self.invocations.get_mut(invocation_id) else {
            return false;
        };
        entry.status = InvocationStatus::Resolved;
        entry.provider_message_ids = provider_message_ids;
        entry.sent_seq = sent_seq;
        entry.resolution_emission_ids = resolution_emission_ids;
        self.delivered_count += 1;
        true
    }

    pub fn invocation_failed(&mut self, invocation_id: &str, error: impl Into<String>) -> bool {
        let error = error.into();
        let Some(entry) = self.invocations.get_mut(invocation_id) else {
            return false;
        };
        entry.status = InvocationStatus::Failed;
        entry.error = Some(error.clone());
        self.failed_delivery_count += 1;
        self.protocol_errors
            .push(format!("invocation {invocation_id}: {error}"));
        true
    }

    pub fn invocation_cancelled(
        &mut self,
        invocation_id: &str,
        resolution_emission_ids: Vec<String>,
    ) -> bool {
        let Some(entry) = self.invocations.get_mut(invocation_id) else {
            return false;
        };
        entry.status = InvocationStatus::Cancelled;
        entry.resolution_emission_ids = resolution_emission_ids;
        true
    }

    fn set_invocation_status(&mut self, invocation_id: &str, status: InvocationStatus) -> bool {
        match self.invocations.get_mut(invocation_id) {
            Some(entry) => {
                entry.status = status;
                true
            }
            None => false,
        }
    }

    /// The bot controller's word on a delivery. A `started` for a known
    /// delivery and a `finished` for a finished one are ignored.
    pub fn record_delivery_receipt(&mut self, receipt: &BotDeliveryReceipt) -> ReceiptEffect {
        let known = self.deliveries.get(&receipt.delivery_id);
        match receipt.phase {
            BotDeliveryPhase::Started => {
                if known.is_some() {
                    return ReceiptEffect::Ignored;
                }
                self.deliveries.insert(
                    receipt.delivery_id.clone(),
                    DeliveryRecord {
                        status: DeliveryStatus::Started,
                        session_id: receipt.session_id.clone(),
                        run_id: receipt.run_id.clone(),
                        outcome: None,
                        fallback: None,
                    },
                );
                ReceiptEffect::Started
            }
            BotDeliveryPhase::Finished => {
                if known.is_some_and(|record| record.status == DeliveryStatus::Finished) {
                    return ReceiptEffect::Ignored;
                }
                self.deliveries.insert(
                    receipt.delivery_id.clone(),
                    DeliveryRecord {
                        status: DeliveryStatus::Finished,
                        session_id: receipt.session_id.clone(),
                        run_id: receipt.run_id.clone(),
                        outcome: receipt.outcome,
                        fallback: None,
                    },
                );
                ReceiptEffect::Finished
            }
        }
    }

    /// Set the fallback of a known delivery; `false` when unknown.
    pub fn set_delivery_fallback(&mut self, delivery_id: &str, fallback: DeliveryFallback) -> bool {
        match self.deliveries.get_mut(delivery_id) {
            Some(record) => {
                record.fallback = Some(fallback);
                true
            }
            None => false,
        }
    }

    /// Deliveries to the provider in flight: invocations received or
    /// delivering, and policy responses delivering.
    pub fn active_deliveries(&self) -> u32 {
        let invocations = self
            .invocations
            .values()
            .filter(|entry| entry.is_active())
            .count();
        let responses = self
            .policy_responses
            .values()
            .filter(|response| response.status == PolicyResponseStatus::Delivering)
            .count();
        u32::try_from(invocations + responses).unwrap_or(u32::MAX)
    }

    /// Typing shows while any bot delivery is started.
    pub fn typing(&self) -> bool {
        self.deliveries
            .values()
            .any(|record| record.status == DeliveryStatus::Started)
    }

    /// The compacted state continue-as-new carries: active work in full,
    /// finished work and dedup ids bounded.
    pub fn compact_state(&self) -> ConversationCarry {
        let mut state = self.clone();
        state.protocol_errors = tail(&self.protocol_errors, MAX_CARRIED_PROTOCOL_ERRORS);
        state.cancellations = tail(&self.cancellations, MAX_CARRIED_CANCELLATIONS);
        state.invocations = self
            .invocations
            .retain_recent(ReceivedInvocation::is_active, MAX_CARRIED_INVOCATIONS);
        state.messages = self.messages.retain_recent(
            |message| message.status == MessageStatus::Emitting,
            MAX_CARRIED_MESSAGES,
        );
        state.deliveries = self
            .deliveries
            .retain_recent(DeliveryRecord::is_active, MAX_CARRIED_DELIVERIES);
        state.policy_responses = self.policy_responses.retain_recent(
            |response| response.status == PolicyResponseStatus::Delivering,
            MAX_CARRIED_POLICY_RESPONSES,
        );
        while state.handles.len() > MAX_CHANNEL_HANDLES {
            state.handles.pop_first();
        }
        state.seen_inbound_ids = self.seen_inbound_ids.newest(MAX_CARRIED_SEEN_IDS);
        state.seen_emission_ids = self.seen_emission_ids.newest(MAX_CARRIED_SEEN_IDS);
        ConversationCarry {
            version: CONVERSATION_CARRY_VERSION,
            state,
        }
    }

    /// The operator-facing snapshot (`channels/conversations/read`).
    pub fn snapshot(&self) -> ChannelConversationSnapshot {
        ChannelConversationSnapshot {
            bot_id: self.bot_id.clone(),
            trigger_id: self.trigger_id.clone(),
            label: self.label.clone(),
            inbound_count: self.inbound_count,
            duplicate_inbound_count: self.duplicate_inbound_count,
            dropped_inbound_count: self.dropped_inbound_count,
            denied_inbound_count: self.denied_inbound_count,
            emitted_count: self.emitted_count,
            delivered_count: self.delivered_count,
            failed_delivery_count: self.failed_delivery_count,
            active_deliveries: self.active_deliveries(),
            typing: self.typing(),
            protocol_errors: self.protocol_errors.clone(),
        }
    }
}

fn tail(values: &[String], limit: usize) -> Vec<String> {
    values[values.len().saturating_sub(limit)..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key doubles as the receipt token stored in a jsonb column, which
    /// rejects U+0000.
    #[test]
    fn inbound_key_is_jsonb_safe() {
        let inbound = crate::inbound::NormalizedInbound {
            provider: api::ChannelProvider::new("telegram"),
            account_id: api::ChannelAccountId::new("tg"),
            inbound: api::ChannelInbound {
                message_id: "m".to_owned(),
                chat_id: "c".to_owned(),
                thread_id: None,
                sender_id: "u".to_owned(),
                sender_name: "U".to_owned(),
                timestamp_ms: 0,
                text: "x".to_owned(),
                media: Vec::new(),
                is_direct: true,
                mentioned_bot: false,
                is_reply_to_bot: false,
            },
        };
        let key = channel_inbound_key(&inbound);
        assert!(!key.contains('\0'));
        assert!(serde_json::to_string(&key).is_ok());
    }
    use api::{BotId, BotTriggerId, ChannelAccountId, ChannelInbound, ChannelProvider};
    use bots::signal::BotDeliveryPhase;
    use uuid::Uuid;

    use crate::ids::ConversationRef;

    const SESSION_ID: &str = "bot:v1:concierge:k-telegram-primary-123-0123abcd";

    fn invocation_id() -> String {
        format!("wti:sha256:{}", "a".repeat(64))
    }

    fn start() -> ConversationStart {
        ConversationStart {
            universe_id: Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").unwrap(),
            bot_id: BotId::new("concierge"),
            trigger_id: BotTriggerId::new("tg"),
            account_id: ChannelAccountId::new("primary"),
            provider: ChannelProvider::new("telegram"),
            conversation: ConversationRef {
                account_id: ChannelAccountId::new("primary"),
                chat_id: "123".to_owned(),
                thread_id: None,
            },
            scope: ChatScope::Direct,
            activation: ChatActivation {
                group: Some(ChatGroupActivation::Always),
                trigger_prefixes: vec!["/ask".to_owned()],
                mention_names: Vec::new(),
            },
            access: ChatAccess::default(),
            label: "telegram dm · Lukas".to_owned(),
            connector_task_queue: "lightspeed-connector-telegram-test".to_owned(),
        }
    }

    fn inbound(message_id: &str) -> NormalizedInbound {
        NormalizedInbound::new(
            ChannelProvider::new("telegram"),
            ChannelAccountId::new("primary"),
            ChannelInbound {
                message_id: message_id.to_owned(),
                chat_id: "123".to_owned(),
                thread_id: None,
                sender_id: "7".to_owned(),
                sender_name: "Lukas".to_owned(),
                timestamp_ms: 1_700_000_000_000,
                text: "hello".to_owned(),
                media: Vec::new(),
                is_direct: true,
                mentioned_bot: false,
                is_reply_to_bot: false,
            },
        )
    }

    fn invocation() -> ConversationEmission {
        ConversationEmission {
            emission_id: invocation_id(),
            producer_session_id: Some(SESSION_ID.to_owned()),
            body: EmissionBody::ToolInvocation {
                holder_workflow_id: format!("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/{SESSION_ID}"),
                invocation_id: invocation_id(),
                tool_id: "channels.message_send.v1".to_owned(),
                arguments_ref: format!("sha256:{}", "c".repeat(64)),
            },
        }
    }

    fn receipt(phase: BotDeliveryPhase, delivery_id: &str) -> BotDeliveryReceipt {
        BotDeliveryReceipt {
            token: "token".to_owned(),
            phase,
            delivery_id: delivery_id.to_owned(),
            seqs: vec![1],
            session_id: SESSION_ID.to_owned(),
            run_id: Some("run_1".to_owned()),
            outcome: (phase == BotDeliveryPhase::Finished).then_some(BotEventOutcome::Handled),
            summary: None,
        }
    }

    #[test]
    fn initial_state_comes_from_the_start() {
        let state = ConversationState::new(&start());
        assert_eq!(state.bot_id, "concierge");
        assert_eq!(state.trigger_id, "tg");
        assert_eq!(state.conversation_key, "primary/123");
        assert_eq!(state.group_activation, ChatGroupActivation::Always);
        assert_eq!(state.label, "telegram dm · Lukas");
        assert_eq!(state.snapshot().bot_id, "concierge");
        assert!(!state.typing());
        assert_eq!(state.active_deliveries(), 0);
    }

    #[test]
    fn deduplicates_inbound_provider_messages() {
        let mut state = ConversationState::new(&start());
        let key = state.apply_inbound(&inbound("42")).unwrap();
        assert_eq!(key, ["telegram", "primary", "123", "", "42"].join("\u{1f}"));
        assert_eq!(state.apply_inbound(&inbound("42")), None);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.inbound_count, 1);
        assert_eq!(snapshot.duplicate_inbound_count, 1);
        let mut threaded = inbound("42");
        threaded.inbound.thread_id = Some("7".to_owned());
        assert!(state.apply_inbound(&threaded).is_some());
    }

    #[test]
    fn keeps_a_bounded_handle_cache_keyed_by_message_number() {
        let mut state = ConversationState::new(&start());
        for seq in 1..=(MAX_CHANNEL_HANDLES as u64 + 4) {
            state.remember_handle(
                seq,
                ChatHandle {
                    provider_message_ids: vec![seq.to_string()],
                    from_me: seq % 2 == 0,
                    sender_id: None,
                    text: None,
                },
            );
        }
        assert_eq!(state.handles.len(), MAX_CHANNEL_HANDLES);
        assert!(!state.handles.contains_key(&1));
        assert!(!state.handles.contains_key(&4));
        assert_eq!(
            state.handles.get(&5),
            Some(&ChatHandle {
                provider_message_ids: vec!["5".to_owned()],
                from_me: false,
                sender_id: None,
                text: None,
            })
        );
        assert!(
            state
                .handles
                .contains_key(&(MAX_CHANNEL_HANDLES as u64 + 4))
        );
    }

    #[test]
    fn sheds_inbound_signals_beyond_the_inbox_ceiling() {
        let mut state = ConversationState::new(&start());
        let mut inbox = Vec::new();
        for index in 0..(MAX_CHANNEL_INBOUND_INBOX + 2) {
            state.enqueue_bounded_inbound(&mut inbox, inbound(&index.to_string()));
        }
        assert_eq!(inbox.len(), MAX_CHANNEL_INBOUND_INBOX);
        assert_eq!(state.overloaded_inbound_count, 2);
    }

    #[test]
    fn deduplicates_pushed_invocations_and_refuses_a_run_terminal() {
        let mut state = ConversationState::new(&start());
        let envelope = invocation();
        assert_eq!(
            state.apply_emission(&envelope),
            ApplyEmissionEffect::InvocationReceived {
                invocation_id: invocation_id(),
            }
        );
        assert_eq!(state.apply_emission(&envelope), ApplyEmissionEffect::None);
        assert_eq!(
            state.apply_emission(&ConversationEmission {
                emission_id: format!("emission:sha256:{}", "e".repeat(64)),
                producer_session_id: Some(SESSION_ID.to_owned()),
                body: EmissionBody::RunTerminal,
            }),
            ApplyEmissionEffect::None
        );
        assert_eq!(
            state.apply_emission(&ConversationEmission {
                emission_id: "workflow-produced".to_owned(),
                producer_session_id: None,
                body: EmissionBody::ToolInvocation {
                    holder_workflow_id: "w".to_owned(),
                    invocation_id: "other".to_owned(),
                    tool_id: "channels.message_send.v1".to_owned(),
                    arguments_ref: "sha256:x".to_owned(),
                },
            }),
            ApplyEmissionEffect::None
        );
        assert_eq!(
            state.invocations.keys().collect::<Vec<_>>(),
            vec![invocation_id()]
        );
        assert_eq!(state.duplicate_emission_count, 1);
        assert_eq!(
            state.protocol_errors,
            vec![
                "conversation received a run terminal it does not own",
                "tool invocation must be produced by a session",
            ]
        );
        let entry = state.invocations.get(&invocation_id()).unwrap();
        assert_eq!(entry.status, InvocationStatus::Received);
        assert_eq!(entry.producer_session_id, SESSION_ID);
        assert_eq!(entry.tool_id, "channels.message_send.v1");
        assert_eq!(state.active_deliveries(), 1);
    }

    #[test]
    fn surfaces_cancellation_effects_independently_of_invocation_delivery() {
        let mut state = ConversationState::new(&start());
        let effect = state.apply_emission(&ConversationEmission {
            emission_id: format!("emission:sha256:{}", "f".repeat(64)),
            producer_session_id: Some(SESSION_ID.to_owned()),
            body: EmissionBody::InvocationCancellation {
                invocation_id: invocation_id(),
                completion_key: "reply".to_owned(),
                promise_id: "promise_1".to_owned(),
            },
        });
        assert_eq!(
            effect,
            ApplyEmissionEffect::InvocationCancelled {
                invocation_id: invocation_id(),
                promise_id: "promise_1".to_owned(),
            }
        );
        assert!(state.is_cancelled(&invocation_id()));
        assert!(!state.is_cancelled("other"));
        assert_eq!(
            state.cancellations,
            vec![format!("{}:reply", invocation_id())]
        );
    }

    #[test]
    fn invocation_transitions_count_deliveries() {
        let mut state = ConversationState::new(&start());
        state.apply_emission(&invocation());
        assert!(state.invocation_delivering(&invocation_id()));
        assert_eq!(state.active_deliveries(), 1);
        assert!(state.invocation_resolved(
            &invocation_id(),
            vec!["m1".to_owned(), "m2".to_owned()],
            Some(9),
            vec!["res-1".to_owned()],
        ));
        let entry = state.invocations.get(&invocation_id()).unwrap();
        assert_eq!(entry.status, InvocationStatus::Resolved);
        assert_eq!(entry.sent_seq, Some(9));
        assert_eq!(entry.provider_message_ids, vec!["m1", "m2"]);
        assert_eq!(state.active_deliveries(), 0);
        assert_eq!(state.snapshot().delivered_count, 1);

        let mut other = invocation();
        other.emission_id = "second".to_owned();
        let EmissionBody::ToolInvocation { invocation_id, .. } = &mut other.body else {
            unreachable!()
        };
        *invocation_id = "second".to_owned();
        state.apply_emission(&other);
        assert!(state.invocation_failed("second", "provider down"));
        assert_eq!(state.snapshot().failed_delivery_count, 1);
        assert_eq!(
            state.protocol_errors,
            vec!["invocation second: provider down"]
        );
        assert!(!state.invocation_failed("missing", "x"));
        assert!(!state.invocation_cancelled("missing", Vec::new()));
    }

    #[test]
    fn records_delivery_receipts_once_per_phase() {
        let mut state = ConversationState::new(&start());
        assert_eq!(
            state.record_delivery_receipt(&receipt(BotDeliveryPhase::Started, "d1")),
            ReceiptEffect::Started
        );
        assert!(state.typing());
        assert!(state.snapshot().typing);
        assert_eq!(
            state.record_delivery_receipt(&receipt(BotDeliveryPhase::Started, "d1")),
            ReceiptEffect::Ignored
        );
        assert_eq!(
            state.record_delivery_receipt(&receipt(BotDeliveryPhase::Finished, "d1")),
            ReceiptEffect::Finished
        );
        assert!(!state.typing());
        assert_eq!(
            state.deliveries.get("d1").unwrap().outcome,
            Some(BotEventOutcome::Handled)
        );
        assert_eq!(
            state.record_delivery_receipt(&receipt(BotDeliveryPhase::Finished, "d1")),
            ReceiptEffect::Ignored
        );
        assert_eq!(
            state.record_delivery_receipt(&receipt(BotDeliveryPhase::Started, "d1")),
            ReceiptEffect::Ignored
        );
        // A finish for an unseen delivery still lands.
        assert_eq!(
            state.record_delivery_receipt(&receipt(BotDeliveryPhase::Finished, "d2")),
            ReceiptEffect::Finished
        );
        assert!(
            state
                .set_delivery_fallback("d2", DeliveryFallback::status(FallbackStatus::Reconciling))
        );
        assert!(state.deliveries.get("d2").unwrap().is_active());
        assert!(
            !state
                .set_delivery_fallback("d9", DeliveryFallback::status(FallbackStatus::Suppressed))
        );
    }

    #[test]
    fn compacts_finished_state_while_retaining_active_work_and_bounded_dedup() {
        let mut state = ConversationState::new(&start());
        state.tools_ref = Some(format!("sha256:{}", "1".repeat(64)));
        for index in 0..300 {
            state.messages.insert(
                format!("done-{index}"),
                ReceivedMessage {
                    message_id: index.to_string(),
                    status: MessageStatus::Emitted,
                    seq: Some(index + 1),
                    session_id: None,
                    error: None,
                },
            );
        }
        state
            .messages
            .insert("pending", ReceivedMessage::emitting("pending"));
        for index in 0..140 {
            state.deliveries.insert(
                format!("d-{index}"),
                DeliveryRecord {
                    status: DeliveryStatus::Finished,
                    session_id: SESSION_ID.to_owned(),
                    run_id: Some(format!("run_{index}")),
                    outcome: Some(BotEventOutcome::Handled),
                    fallback: Some(DeliveryFallback::status(FallbackStatus::Suppressed)),
                },
            );
        }
        state.deliveries.insert(
            "open",
            DeliveryRecord {
                status: DeliveryStatus::Started,
                session_id: SESSION_ID.to_owned(),
                run_id: Some("run_x".to_owned()),
                outcome: None,
                fallback: None,
            },
        );
        for index in 0..2_100 {
            state.seen_inbound_ids.insert(format!("inbound-{index}"));
            state.seen_emission_ids.insert(format!("emission-{index}"));
        }
        for index in 0..40 {
            state.protocol_errors.push(format!("error {index}"));
        }
        for seq in 1..=600 {
            state.remember_handle(
                seq,
                ChatHandle {
                    provider_message_ids: vec![seq.to_string()],
                    from_me: false,
                    sender_id: None,
                    text: None,
                },
            );
        }

        let carry = state.compact_state();
        assert_eq!(carry.version, CONVERSATION_CARRY_VERSION);
        let compacted = &carry.state;
        assert_eq!(compacted.messages.len(), 257);
        assert_eq!(
            compacted
                .messages
                .get("pending")
                .map(|message| message.status),
            Some(MessageStatus::Emitting)
        );
        assert!(compacted.messages.get("done-0").is_none());
        assert!(compacted.messages.get("done-44").is_some());
        assert_eq!(compacted.messages.keys().last(), Some("pending"));
        assert_eq!(compacted.deliveries.len(), 129);
        assert_eq!(
            compacted
                .deliveries
                .get("open")
                .map(|delivery| delivery.status),
            Some(DeliveryStatus::Started)
        );
        assert_eq!(compacted.tools_ref, state.tools_ref);
        assert_eq!(compacted.seen_inbound_ids.len(), 2_048);
        assert_eq!(compacted.seen_inbound_ids.iter().next(), Some("inbound-52"));
        assert!(compacted.seen_inbound_ids.contains("inbound-2099"));
        assert!(!compacted.seen_inbound_ids.contains("inbound-51"));
        assert_eq!(compacted.seen_emission_ids.len(), 2_048);
        assert_eq!(compacted.protocol_errors.len(), MAX_CARRIED_PROTOCOL_ERRORS);
        assert_eq!(compacted.protocol_errors[0], "error 8");
        assert_eq!(compacted.handles.len(), MAX_CHANNEL_HANDLES);
        assert!(compacted.handles.contains_key(&600));
        // The source is untouched.
        assert_eq!(state.messages.len(), 301);

        let restored = ConversationState::restore(&start(), carry.clone()).unwrap();
        assert_eq!(restored, carry.state);
        let mut other = start();
        other.conversation.chat_id = "other".to_owned();
        assert!(matches!(
            ConversationState::restore(&other, carry.clone()),
            Err(ChannelError::InvalidInput { message }) if message.contains("does not match")
        ));
        let mut other_trigger = start();
        other_trigger.trigger_id = BotTriggerId::new("wa");
        assert!(ConversationState::restore(&other_trigger, carry.clone()).is_err());
        let stale = ConversationCarry {
            version: 0,
            ..carry
        };
        assert!(ConversationState::restore(&start(), stale).is_err());
    }

    #[test]
    fn carry_round_trips_through_json_in_order() {
        let mut state = ConversationState::new(&start());
        state.apply_inbound(&inbound("b"));
        state.apply_inbound(&inbound("a"));
        state.messages.insert("z", ReceivedMessage::emitting("z"));
        state.messages.insert("a", ReceivedMessage::emitting("a"));
        state.apply_emission(&invocation());
        state.remember_handle(
            3,
            ChatHandle {
                provider_message_ids: vec!["p3".to_owned()],
                from_me: true,
                sender_id: Some("me".to_owned()),
                text: Some("hi".to_owned()),
            },
        );
        state.record_delivery_receipt(&receipt(BotDeliveryPhase::Started, "d1"));
        state.policy_responses.insert(
            "k",
            PolicyResponse {
                kind: PolicyResponseKind::Control,
                status: PolicyResponseStatus::Delivering,
                provider_message_ids: Vec::new(),
                error: None,
            },
        );
        let carry = state.compact_state();
        let json = serde_json::to_value(&carry).unwrap();
        assert_eq!(json["state"]["groupActivation"], "always");
        assert_eq!(
            json["state"]["seenInboundIds"][0],
            ["telegram", "primary", "123", "", "b"].join("\u{1f}")
        );
        let message_keys: Vec<&str> = json["state"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry[0].as_str().unwrap())
            .collect();
        assert_eq!(message_keys, vec!["z", "a"]);
        assert_eq!(json["state"]["messages"][0][1]["status"], "emitting");
        let back: ConversationCarry = serde_json::from_value(json).unwrap();
        assert_eq!(back, carry);
        assert_eq!(
            back.state.messages.keys().collect::<Vec<_>>(),
            vec!["z", "a"]
        );
        assert_eq!(back.state.active_deliveries(), 2);
        assert!(back.state.typing());
    }

    #[test]
    fn ordered_map_keeps_positions_on_replace_and_retains_recent() {
        let mut map = OrderedMap::new();
        map.insert("a", 1);
        map.insert("b", 2);
        map.insert("c", 3);
        assert_eq!(map.insert("a", 10), Some(1));
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["a", "b", "c"]);
        assert_eq!(map.remove("b"), Some(2));
        assert_eq!(map.remove("b"), None);
        assert_eq!(map.get("a"), Some(&10));
        map.insert("d", 4);
        map.insert("e", 5);
        let retained = map.retain_recent(|value| *value == 10, 2);
        assert_eq!(retained.keys().collect::<Vec<_>>(), vec!["d", "e", "a"]);
        assert!(map.contains_key("c"));
        assert!(!retained.contains_key("c"));
    }
}
