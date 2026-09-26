//! In-memory bot registry for tests: the three store traits of
//! [`crate::records`] over `BTreeMap`s behind one `RwLock`. The semantics
//! pinned by the tests at the bottom are the contract the PostgreSQL
//! adapter in `store-pg` is checked against.

use std::{
    collections::BTreeMap,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use api::{
    BotDocument, BotId, BotTriggerDisabledReason, BotTriggerId, BotTriggerKind, PollCursorState,
    ProfileId,
};
use async_trait::async_trait;

use crate::{
    BotError,
    records::{
        BotEventCursor, BotEventOutcomeWrite, BotEventRateScope, BotEventRecord, BotEventStore,
        BotRecord, BotRosterRow, BotStore, BotTriggerRecord, BotTriggerStore, BotTriggerWrite,
        InsertBotEventOutcome,
    },
    validate::{validate_bot_document, validate_trigger_document},
};

type TriggerKey = (BotId, BotTriggerId);
type EventKey = (BotId, String);
type SeqKey = (BotId, u64);

#[derive(Default)]
struct State {
    bots: BTreeMap<BotId, BotRecord>,
    /// Keyed `(bot, trigger)`, so iteration is the documented
    /// bot-id-then-trigger-id order.
    triggers: BTreeMap<TriggerKey, BotTriggerRecord>,
    events: BTreeMap<EventKey, BotEventRecord>,
    /// `(bot, seq)` → event id: `#N` lookups and the roster's latest event.
    seqs: BTreeMap<SeqKey, String>,
}

pub struct InMemoryBotStore {
    state: RwLock<State>,
}

impl Default for InMemoryBotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryBotStore {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(State::default()),
        }
    }

    fn read_state(&self) -> Result<RwLockReadGuard<'_, State>, BotError> {
        self.state
            .read()
            .map_err(|_| BotError::store("bot store read lock poisoned"))
    }

    fn write_state(&self) -> Result<RwLockWriteGuard<'_, State>, BotError> {
        self.state
            .write()
            .map_err(|_| BotError::store("bot store write lock poisoned"))
    }
}

fn bot_not_found(bot_id: &BotId) -> BotError {
    BotError::BotNotFound {
        bot_id: bot_id.clone(),
    }
}

fn trigger_not_found(bot_id: &BotId, trigger_id: &BotTriggerId) -> BotError {
    BotError::TriggerNotFound {
        bot_id: bot_id.clone(),
        trigger_id: trigger_id.clone(),
    }
}

/// Whether `next` differs from `current` in nothing but the human labels
/// (`display_name`, `description`) — the only edit a closed bot accepts.
fn only_labels_differ(current: &BotDocument, next: &BotDocument) -> bool {
    let relabeled = BotDocument {
        display_name: next.display_name.clone(),
        description: next.description.clone(),
        ..current.clone()
    };
    relabeled == *next
}

fn apply_disable(record: &mut BotTriggerRecord, reason: BotTriggerDisabledReason, now_ms: i64) {
    record.document.enabled = false;
    record.disabled_reason = Some(reason);
    record.disabled_at_ms = Some(now_ms);
    record.revision += 1;
    record.updated_at_ms = now_ms;
}

impl State {
    fn triggers_of<'a>(
        &'a self,
        bot_id: &'a BotId,
    ) -> impl Iterator<Item = &'a BotTriggerRecord> + 'a {
        self.triggers
            .iter()
            .filter(move |((owner, _), _)| owner == bot_id)
            .map(|(_, record)| record)
    }

    fn events_of<'a>(&'a self, bot_id: &'a BotId) -> impl Iterator<Item = &'a BotEventRecord> + 'a {
        self.events
            .iter()
            .filter(move |((owner, _), _)| owner == bot_id)
            .map(|(_, record)| record)
    }

    fn last_event_of(&self, bot_id: &BotId) -> Option<BotEventRecord> {
        let range = (bot_id.clone(), 0)..=(bot_id.clone(), u64::MAX);
        let (_, event_id) = self.seqs.range(range).next_back()?;
        self.events
            .get(&(bot_id.clone(), event_id.clone()))
            .cloned()
    }
}

#[async_trait]
impl BotStore for InMemoryBotStore {
    async fn create_bot(
        &self,
        bot_id: BotId,
        document: BotDocument,
        now_ms: i64,
    ) -> Result<BotRecord, BotError> {
        validate_bot_document(&document)?;
        let mut state = self.write_state()?;
        if state.bots.contains_key(&bot_id) {
            return Err(BotError::BotAlreadyExists { bot_id });
        }
        let record = BotRecord {
            bot_id: bot_id.clone(),
            revision: 1,
            document,
            event_seq: 0,
            closed_at_ms: None,
            closed_sessions: Vec::new(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        state.bots.insert(bot_id, record.clone());
        Ok(record)
    }

    async fn put_bot(
        &self,
        bot_id: BotId,
        document: BotDocument,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<BotRecord, BotError> {
        validate_bot_document(&document)?;
        let mut state = self.write_state()?;
        let record = state
            .bots
            .get_mut(&bot_id)
            .ok_or_else(|| bot_not_found(&bot_id))?;
        if let Some(expected) = expected_revision
            && expected != record.revision
        {
            return Err(BotError::BotRevisionConflict {
                bot_id,
                expected,
                actual: record.revision,
            });
        }
        if record.is_closed() && !only_labels_differ(&record.document, &document) {
            return Err(BotError::BotClosed { bot_id });
        }
        record.document = document;
        record.revision += 1;
        record.updated_at_ms = now_ms;
        Ok(record.clone())
    }

    async fn read_bot(&self, bot_id: &BotId) -> Result<BotRecord, BotError> {
        self.read_state()?
            .bots
            .get(bot_id)
            .cloned()
            .ok_or_else(|| bot_not_found(bot_id))
    }

    async fn list_bots(&self) -> Result<Vec<BotRecord>, BotError> {
        Ok(self.read_state()?.bots.values().cloned().collect())
    }

    async fn list_bot_roster(&self) -> Result<Vec<BotRosterRow>, BotError> {
        let state = self.read_state()?;
        Ok(state
            .bots
            .values()
            .map(|bot| BotRosterRow {
                bot: bot.clone(),
                trigger_count: u32::try_from(state.triggers_of(&bot.bot_id).count())
                    .unwrap_or(u32::MAX),
                pending_count: state
                    .events_of(&bot.bot_id)
                    .filter(|event| event.is_pending())
                    .count() as u64,
                last_event: state.last_event_of(&bot.bot_id),
                // This store holds no sessions.
                activity: api::SessionActivity::Idle,
            })
            .collect())
    }

    async fn list_bots_for_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<Vec<BotRecord>, BotError> {
        Ok(self
            .read_state()?
            .bots
            .values()
            .filter(|bot| !bot.is_closed() && &bot.document.profile_id == profile_id)
            .cloned()
            .collect())
    }

    async fn close_bot(&self, bot_id: &BotId, now_ms: i64) -> Result<BotRecord, BotError> {
        let mut state = self.write_state()?;
        let record = state
            .bots
            .get_mut(bot_id)
            .ok_or_else(|| bot_not_found(bot_id))?;
        if record.is_closed() {
            return Ok(record.clone());
        }
        record.closed_at_ms = Some(now_ms);
        record.document.enabled = false;
        record.revision += 1;
        record.updated_at_ms = now_ms;
        Ok(record.clone())
    }

    async fn record_bot_closed_sessions(
        &self,
        bot_id: &BotId,
        sessions: Vec<String>,
    ) -> Result<Vec<String>, BotError> {
        let mut state = self.write_state()?;
        let record = state
            .bots
            .get_mut(bot_id)
            .ok_or_else(|| bot_not_found(bot_id))?;
        for session in sessions {
            if !record.closed_sessions.contains(&session) {
                record.closed_sessions.push(session);
            }
        }
        Ok(record.closed_sessions.clone())
    }

    async fn delete_bot(&self, bot_id: &BotId) -> Result<BotRecord, BotError> {
        let mut state = self.write_state()?;
        let record = state
            .bots
            .remove(bot_id)
            .ok_or_else(|| bot_not_found(bot_id))?;
        state.triggers.retain(|(owner, _), _| owner != bot_id);
        state.events.retain(|(owner, _), _| owner != bot_id);
        state.seqs.retain(|(owner, _), _| owner != bot_id);
        Ok(record)
    }

    async fn allocate_bot_event_seq(&self, bot_id: &BotId) -> Result<u64, BotError> {
        let mut state = self.write_state()?;
        let record = state
            .bots
            .get_mut(bot_id)
            .ok_or_else(|| bot_not_found(bot_id))?;
        record.event_seq += 1;
        Ok(record.event_seq)
    }
}

#[async_trait]
impl BotTriggerStore for InMemoryBotStore {
    async fn put_bot_trigger(
        &self,
        bot_id: &BotId,
        write: BotTriggerWrite,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<BotTriggerRecord, BotError> {
        validate_trigger_document(&write.document, now_ms)?;
        let mut state = self.write_state()?;
        if !state.bots.contains_key(bot_id) {
            return Err(bot_not_found(bot_id));
        }
        let key = (bot_id.clone(), write.trigger_id.clone());
        let record = match state.triggers.get(&key) {
            Some(existing) => {
                if let Some(expected) = expected_revision
                    && expected != existing.revision
                {
                    return Err(BotError::TriggerRevisionConflict {
                        bot_id: bot_id.clone(),
                        trigger_id: write.trigger_id,
                        expected,
                        actual: existing.revision,
                    });
                }
                // Re-enabling through the document is the way out of a
                // runtime disable; the filter incident stays until the
                // next match clears it.
                let re_enabled = write.document.enabled;
                BotTriggerRecord {
                    bot_id: bot_id.clone(),
                    trigger_id: write.trigger_id,
                    revision: existing.revision + 1,
                    document: write.document,
                    secrets: write.secrets,
                    disabled_reason: if re_enabled {
                        None
                    } else {
                        existing.disabled_reason
                    },
                    disabled_at_ms: if re_enabled {
                        None
                    } else {
                        existing.disabled_at_ms
                    },
                    last_filter_error: existing.last_filter_error.clone(),
                    last_filter_error_at_ms: existing.last_filter_error_at_ms,
                    cursor: match write.cursor {
                        Some(cursor) => cursor,
                        None => existing.cursor.clone(),
                    },
                    created_at_ms: existing.created_at_ms,
                    updated_at_ms: now_ms,
                }
            }
            None => BotTriggerRecord {
                bot_id: bot_id.clone(),
                trigger_id: write.trigger_id,
                revision: 1,
                document: write.document,
                secrets: write.secrets,
                disabled_reason: None,
                disabled_at_ms: None,
                last_filter_error: None,
                last_filter_error_at_ms: None,
                cursor: write.cursor.unwrap_or(None),
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            },
        };
        state.triggers.insert(key, record.clone());
        Ok(record)
    }

    async fn read_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, BotError> {
        self.read_state()?
            .triggers
            .get(&(bot_id.clone(), trigger_id.clone()))
            .cloned()
            .ok_or_else(|| trigger_not_found(bot_id, trigger_id))
    }

    async fn list_bot_triggers(&self, bot_id: &BotId) -> Result<Vec<BotTriggerRecord>, BotError> {
        Ok(self.read_state()?.triggers_of(bot_id).cloned().collect())
    }

    async fn list_bot_triggers_by_kind(
        &self,
        kind: BotTriggerKind,
    ) -> Result<Vec<BotTriggerRecord>, BotError> {
        Ok(self
            .read_state()?
            .triggers
            .values()
            .filter(|record| record.kind() == kind)
            .cloned()
            .collect())
    }

    async fn delete_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, BotError> {
        self.write_state()?
            .triggers
            .remove(&(bot_id.clone(), trigger_id.clone()))
            .ok_or_else(|| trigger_not_found(bot_id, trigger_id))
    }

    async fn disable_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        reason: BotTriggerDisabledReason,
        now_ms: i64,
    ) -> Result<BotTriggerRecord, BotError> {
        let mut state = self.write_state()?;
        let record = state
            .triggers
            .get_mut(&(bot_id.clone(), trigger_id.clone()))
            .ok_or_else(|| trigger_not_found(bot_id, trigger_id))?;
        apply_disable(record, reason, now_ms);
        Ok(record.clone())
    }

    async fn disable_bot_triggers(
        &self,
        bot_id: &BotId,
        reason: BotTriggerDisabledReason,
        now_ms: i64,
    ) -> Result<Vec<BotTriggerRecord>, BotError> {
        let mut state = self.write_state()?;
        let mut changed = Vec::new();
        for ((owner, _), record) in state.triggers.iter_mut() {
            if owner != bot_id || !record.enabled() {
                continue;
            }
            apply_disable(record, reason, now_ms);
            changed.push(record.clone());
        }
        Ok(changed)
    }

    async fn set_bot_trigger_filter_error(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        error: Option<String>,
        now_ms: i64,
    ) -> Result<(), BotError> {
        let mut state = self.write_state()?;
        let record = state
            .triggers
            .get_mut(&(bot_id.clone(), trigger_id.clone()))
            .ok_or_else(|| trigger_not_found(bot_id, trigger_id))?;
        record.last_filter_error_at_ms = error.as_ref().map(|_| now_ms);
        record.last_filter_error = error;
        Ok(())
    }

    async fn set_bot_trigger_cursor(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        cursor: Option<PollCursorState>,
    ) -> Result<(), BotError> {
        let mut state = self.write_state()?;
        let record = state
            .triggers
            .get_mut(&(bot_id.clone(), trigger_id.clone()))
            .ok_or_else(|| trigger_not_found(bot_id, trigger_id))?;
        record.cursor = cursor;
        Ok(())
    }
}

#[async_trait]
impl BotEventStore for InMemoryBotStore {
    async fn insert_bot_event(
        &self,
        record: BotEventRecord,
    ) -> Result<InsertBotEventOutcome, BotError> {
        let mut state = self.write_state()?;
        if !state.bots.contains_key(&record.bot_id) {
            return Err(bot_not_found(&record.bot_id));
        }
        let key = (record.bot_id.clone(), record.event_id.clone());
        if let Some(stored) = state.events.get(&key) {
            return Ok(InsertBotEventOutcome::Duplicate(stored.clone()));
        }
        let seq_key = (record.bot_id.clone(), record.seq);
        if state.seqs.contains_key(&seq_key) {
            return Err(BotError::invalid(format!(
                "bot event #{} of {} already exists",
                record.seq, record.bot_id
            )));
        }
        state.seqs.insert(seq_key, record.event_id.clone());
        state.events.insert(key, record.clone());
        Ok(InsertBotEventOutcome::Inserted(record))
    }

    async fn delete_bot_event(&self, bot_id: &BotId, event_id: &str) -> Result<bool, BotError> {
        let mut state = self.write_state()?;
        let Some(removed) = state.events.remove(&(bot_id.clone(), event_id.to_owned())) else {
            return Ok(false);
        };
        state.seqs.remove(&(bot_id.clone(), removed.seq));
        Ok(true)
    }

    async fn read_bot_event_by_seq(
        &self,
        bot_id: &BotId,
        seq: u64,
    ) -> Result<BotEventRecord, BotError> {
        let state = self.read_state()?;
        state
            .seqs
            .get(&(bot_id.clone(), seq))
            .and_then(|event_id| state.events.get(&(bot_id.clone(), event_id.clone())))
            .cloned()
            .ok_or_else(|| BotError::EventNotFound {
                bot_id: bot_id.clone(),
                seq,
            })
    }

    async fn read_bot_event(
        &self,
        bot_id: &BotId,
        event_id: &str,
    ) -> Result<BotEventRecord, BotError> {
        self.read_state()?
            .events
            .get(&(bot_id.clone(), event_id.to_owned()))
            .cloned()
            .ok_or_else(|| BotError::EventIdNotFound {
                bot_id: bot_id.clone(),
                event_id: event_id.to_owned(),
            })
    }

    async fn read_bot_events(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
    ) -> Result<Vec<BotEventRecord>, BotError> {
        let state = self.read_state()?;
        // Keyed by seq: log order, and a repeated id appears once.
        let found: BTreeMap<u64, &BotEventRecord> = event_ids
            .iter()
            .filter_map(|event_id| state.events.get(&(bot_id.clone(), event_id.clone())))
            .map(|record| (record.seq, record))
            .collect();
        Ok(found.into_values().cloned().collect())
    }

    async fn list_bot_events(
        &self,
        bot_id: &BotId,
        limit: usize,
        before: Option<BotEventCursor>,
    ) -> Result<Vec<BotEventRecord>, BotError> {
        let state = self.read_state()?;
        let mut page: Vec<&BotEventRecord> = state
            .events_of(bot_id)
            .filter(|event| {
                before.is_none_or(|cursor| {
                    (event.received_at_ms, event.seq) < (cursor.received_at_ms, cursor.seq)
                })
            })
            .collect();
        page.sort_by_key(|event| std::cmp::Reverse((event.received_at_ms, event.seq)));
        Ok(page.into_iter().take(limit).cloned().collect())
    }

    async fn count_bot_events_since(
        &self,
        scope: BotEventRateScope<'_>,
        since_ms: i64,
    ) -> Result<u64, BotError> {
        let state = self.read_state()?;
        let recent = |event: &&BotEventRecord| event.received_at_ms >= since_ms;
        let count = match scope {
            BotEventRateScope::Trigger { bot_id, trigger_id } => state
                .events_of(bot_id)
                .filter(|event| event.trigger_id.as_ref() == Some(trigger_id))
                .filter(recent)
                .count(),
            BotEventRateScope::Sender { sender_bot_id } => state
                .events
                .values()
                .filter(|event| event.sender_bot_id.as_ref() == Some(sender_bot_id))
                .filter(recent)
                .count(),
        };
        Ok(count as u64)
    }

    async fn record_bot_event_outcomes(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
        write: BotEventOutcomeWrite,
    ) -> Result<u64, BotError> {
        let mut state = self.write_state()?;
        let mut changed = 0;
        for event_id in event_ids {
            let Some(record) = state.events.get_mut(&(bot_id.clone(), event_id.clone())) else {
                continue;
            };
            if record.outcome.is_some() {
                continue;
            }
            record.outcome = Some(write.outcome);
            record.outcome_detail = write.detail.clone();
            record.run_id = write.run_id.clone();
            record.resolved_at_ms = Some(write.resolved_at_ms);
            changed += 1;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use api::{
        BotEventOutcome, BotTriggerDocument, BotTriggerSpec, PollCursorSpec, PollSource,
        WebhookVerification,
    };

    use super::*;
    use crate::records::BotTriggerSecrets;

    const T0: i64 = 1_700_000_000_000;

    fn bot(value: &str) -> BotId {
        BotId::new(value)
    }

    fn trigger(value: &str) -> BotTriggerId {
        BotTriggerId::new(value)
    }

    fn document(profile: &str) -> BotDocument {
        BotDocument {
            display_name: Some("Triage".to_owned()),
            description: None,
            profile_id: ProfileId::new(profile),
            brief: Some("watch the queue".to_owned()),
            runs_per_day: None,
            breaker: None,
            routed_session_close_after_ms: None,
            self_config: false,
            emit: false,
            enabled: true,
        }
    }

    fn trigger_document(spec: BotTriggerSpec) -> BotTriggerDocument {
        BotTriggerDocument {
            spec,
            filter: None,
            route: None,
            coalesce: None,
            deliver: None,
            session_close_after_ms: None,
            enabled: true,
        }
    }

    fn schedule() -> BotTriggerDocument {
        trigger_document(BotTriggerSpec::Schedule {
            cron: Some("@hourly".to_owned()),
            at_ms: None,
            timezone: "UTC".to_owned(),
            summary: "check the queue".to_owned(),
        })
    }

    fn webhook() -> BotTriggerDocument {
        trigger_document(BotTriggerSpec::Webhook {
            verification: WebhookVerification::Token,
            preset: None,
        })
    }

    fn poll() -> BotTriggerDocument {
        trigger_document(BotTriggerSpec::Poll {
            source: PollSource::Http {
                url: "https://example.com/feed".to_owned(),
                method: Default::default(),
                headers: BTreeMap::new(),
                auth: None,
                body: None,
            },
            interval_ms: 60_000,
            items: None,
            cursor: PollCursorSpec::IdSet {
                id: "id".to_owned(),
            },
        })
    }

    fn inbox() -> BotTriggerDocument {
        trigger_document(BotTriggerSpec::Bot { from: None })
    }

    fn write(trigger_id: &str, document: BotTriggerDocument) -> BotTriggerWrite {
        BotTriggerWrite {
            trigger_id: trigger(trigger_id),
            document,
            secrets: BotTriggerSecrets::default(),
            cursor: None,
        }
    }

    fn cursor_state(ids: &[&str]) -> PollCursorState {
        PollCursorState {
            ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn event(bot_id: &BotId, event_id: &str, seq: u64, received_at_ms: i64) -> BotEventRecord {
        BotEventRecord {
            bot_id: bot_id.clone(),
            event_id: event_id.to_owned(),
            seq,
            trigger_id: None,
            kind: "webhook".to_owned(),
            summary: format!("event {event_id}"),
            occurred_at_ms: received_at_ms,
            received_at_ms,
            document_ref: format!("doc-{event_id}"),
            prompt_ref: None,
            session: None,
            sender_bot_id: None,
            hops: 0,
            in_reply_to: None,
            media: Vec::new(),
            receiver: None,
            outcome: None,
            outcome_detail: None,
            run_id: None,
            resolved_at_ms: None,
        }
    }

    fn outcome(kind: BotEventOutcome, resolved_at_ms: i64) -> BotEventOutcomeWrite {
        BotEventOutcomeWrite {
            outcome: kind,
            detail: Some("done".to_owned()),
            run_id: Some("run-1".to_owned()),
            resolved_at_ms,
        }
    }

    async fn store_with(bots: &[&str]) -> InMemoryBotStore {
        let store = InMemoryBotStore::new();
        for name in bots {
            store
                .create_bot(bot(name), document("triage"), T0)
                .await
                .unwrap();
        }
        store
    }

    async fn insert(store: &InMemoryBotStore, record: BotEventRecord) -> BotEventRecord {
        match store.insert_bot_event(record).await.unwrap() {
            InsertBotEventOutcome::Inserted(record) => record,
            InsertBotEventOutcome::Duplicate(record) => {
                panic!("unexpected duplicate for #{}", record.seq)
            }
        }
    }

    fn ids(records: &[BotTriggerRecord]) -> Vec<(String, String)> {
        records
            .iter()
            .map(|record| {
                (
                    record.bot_id.as_str().to_owned(),
                    record.trigger_id.as_str().to_owned(),
                )
            })
            .collect()
    }

    fn seqs(records: &[BotEventRecord]) -> Vec<u64> {
        records.iter().map(|record| record.seq).collect()
    }

    // ── Bots ────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn create_bot_starts_at_revision_one_and_refuses_duplicates() {
        let store = InMemoryBotStore::new();
        let record = store
            .create_bot(bot("triage"), document("triage"), T0)
            .await
            .unwrap();
        assert_eq!(record.revision, 1);
        assert_eq!(record.event_seq, 0);
        assert_eq!(record.closed_at_ms, None);
        assert!(record.closed_sessions.is_empty());
        assert_eq!(record.created_at_ms, T0);
        assert_eq!(record.updated_at_ms, T0);
        assert_eq!(store.read_bot(&bot("triage")).await.unwrap(), record);

        let error = store
            .create_bot(bot("triage"), document("other"), T0 + 1)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            BotError::BotAlreadyExists {
                bot_id: bot("triage")
            }
        );
        assert_eq!(
            store.read_bot(&bot("missing")).await.unwrap_err(),
            BotError::BotNotFound {
                bot_id: bot("missing")
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_and_put_validate_the_document() {
        let store = store_with(&["triage"]).await;
        let mut invalid = document("triage");
        invalid.runs_per_day = Some(0);
        assert!(matches!(
            store
                .create_bot(bot("other"), invalid.clone(), T0)
                .await
                .unwrap_err(),
            BotError::InvalidInput { .. }
        ));
        assert!(store.read_bot(&bot("other")).await.is_err());
        assert!(matches!(
            store
                .put_bot(bot("triage"), invalid, None, T0 + 1)
                .await
                .unwrap_err(),
            BotError::InvalidInput { .. }
        ));
        assert_eq!(store.read_bot(&bot("triage")).await.unwrap().revision, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_bot_bumps_revision_and_checks_expected_revision() {
        let store = store_with(&["triage"]).await;
        let mut next = document("triage");
        next.brief = Some("rev 2".to_owned());
        let record = store
            .put_bot(bot("triage"), next.clone(), Some(1), T0 + 10)
            .await
            .unwrap();
        assert_eq!(record.revision, 2);
        assert_eq!(record.document, next);
        assert_eq!(record.created_at_ms, T0);
        assert_eq!(record.updated_at_ms, T0 + 10);

        let error = store
            .put_bot(bot("triage"), next.clone(), Some(1), T0 + 20)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            BotError::BotRevisionConflict {
                bot_id: bot("triage"),
                expected: 1,
                actual: 2,
            }
        );
        assert_eq!(store.read_bot(&bot("triage")).await.unwrap().revision, 2);

        // No expectation: unconditional replace.
        let record = store
            .put_bot(bot("triage"), next, None, T0 + 30)
            .await
            .unwrap();
        assert_eq!(record.revision, 3);

        assert_eq!(
            store
                .put_bot(bot("missing"), document("triage"), None, T0)
                .await
                .unwrap_err(),
            BotError::BotNotFound {
                bot_id: bot("missing")
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_bot_disables_once_and_is_idempotent() {
        let store = store_with(&["triage"]).await;
        let closed = store.close_bot(&bot("triage"), T0 + 5).await.unwrap();
        assert_eq!(closed.closed_at_ms, Some(T0 + 5));
        assert!(!closed.document.enabled);
        assert_eq!(closed.revision, 2);
        assert_eq!(closed.updated_at_ms, T0 + 5);
        assert!(closed.is_closed());

        let again = store.close_bot(&bot("triage"), T0 + 99).await.unwrap();
        assert_eq!(again, closed, "a second close changes nothing");
        assert_eq!(store.read_bot(&bot("triage")).await.unwrap(), closed);
        assert_eq!(
            store.close_bot(&bot("missing"), T0).await.unwrap_err(),
            BotError::BotNotFound {
                bot_id: bot("missing")
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_bot_accepts_only_label_changes() {
        let store = store_with(&["triage"]).await;
        let closed = store.close_bot(&bot("triage"), T0 + 5).await.unwrap();

        let mut relabeled = closed.document.clone();
        relabeled.display_name = Some("Triage (closed)".to_owned());
        relabeled.description = Some("retired".to_owned());
        let record = store
            .put_bot(bot("triage"), relabeled.clone(), Some(2), T0 + 6)
            .await
            .unwrap();
        assert_eq!(record.revision, 3);
        assert_eq!(record.document, relabeled);
        assert!(record.is_closed());

        for mutate in [
            |document: &mut BotDocument| document.brief = Some("new brief".to_owned()),
            |document: &mut BotDocument| document.enabled = true,
            |document: &mut BotDocument| document.profile_id = ProfileId::new("other"),
            |document: &mut BotDocument| document.emit = true,
        ] {
            let mut next = relabeled.clone();
            mutate(&mut next);
            let error = store
                .put_bot(bot("triage"), next, None, T0 + 7)
                .await
                .unwrap_err();
            assert_eq!(
                error,
                BotError::BotClosed {
                    bot_id: bot("triage")
                }
            );
        }
        assert_eq!(store.read_bot(&bot("triage")).await.unwrap(), record);

        // A stale revision is reported before the closed check.
        assert!(matches!(
            store
                .put_bot(bot("triage"), document("triage"), Some(1), T0)
                .await
                .unwrap_err(),
            BotError::BotRevisionConflict { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_bot_closed_sessions_unions() {
        let store = store_with(&["triage"]).await;
        let sessions = store
            .record_bot_closed_sessions(
                &bot("triage"),
                vec!["s-1".to_owned(), "s-2".to_owned(), "s-1".to_owned()],
            )
            .await
            .unwrap();
        assert_eq!(sessions, vec!["s-1", "s-2"]);
        let sessions = store
            .record_bot_closed_sessions(&bot("triage"), vec!["s-2".to_owned(), "s-3".to_owned()])
            .await
            .unwrap();
        assert_eq!(sessions, vec!["s-1", "s-2", "s-3"]);
        let record = store.read_bot(&bot("triage")).await.unwrap();
        assert_eq!(record.closed_sessions, vec!["s-1", "s-2", "s-3"]);
        assert_eq!(record.revision, 1, "not a document change");
        assert!(matches!(
            store
                .record_bot_closed_sessions(&bot("missing"), vec![])
                .await
                .unwrap_err(),
            BotError::BotNotFound { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allocate_bot_event_seq_increments_from_one() {
        let store = store_with(&["triage"]).await;
        assert_eq!(
            store.allocate_bot_event_seq(&bot("triage")).await.unwrap(),
            1
        );
        assert_eq!(
            store.allocate_bot_event_seq(&bot("triage")).await.unwrap(),
            2
        );
        assert_eq!(
            store.allocate_bot_event_seq(&bot("triage")).await.unwrap(),
            3
        );
        let record = store.read_bot(&bot("triage")).await.unwrap();
        assert_eq!(record.event_seq, 3);
        assert_eq!(record.revision, 1, "not a document change");
        assert!(matches!(
            store
                .allocate_bot_event_seq(&bot("missing"))
                .await
                .unwrap_err(),
            BotError::BotNotFound { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_bots_is_ordered_by_id_and_profile_lookup_skips_closed() {
        let store = InMemoryBotStore::new();
        for (name, profile) in [("zeta", "triage"), ("alpha", "triage"), ("mid", "other")] {
            store
                .create_bot(bot(name), document(profile), T0)
                .await
                .unwrap();
        }
        let names: Vec<_> = store
            .list_bots()
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.bot_id.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);

        store.close_bot(&bot("zeta"), T0 + 1).await.unwrap();
        let for_profile: Vec<_> = store
            .list_bots_for_profile(&ProfileId::new("triage"))
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.bot_id.as_str().to_owned())
            .collect();
        assert_eq!(for_profile, vec!["alpha"]);
        assert!(
            store
                .list_bots_for_profile(&ProfileId::new("unused"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn roster_counts_triggers_pending_events_and_latest_seq() {
        let store = store_with(&["alpha", "beta"]).await;
        let alpha = bot("alpha");
        store
            .put_bot_trigger(&alpha, write("hourly", schedule()), None, T0)
            .await
            .unwrap();
        store
            .put_bot_trigger(&alpha, write("hook", webhook()), None, T0)
            .await
            .unwrap();
        // #2 arrived before #1 by wall clock; the roster shows the highest
        // seq, not the newest receipt.
        insert(&store, event(&alpha, "e-1", 1, T0 + 50)).await;
        insert(&store, event(&alpha, "e-2", 2, T0 + 10)).await;
        insert(&store, event(&alpha, "e-3", 3, T0 + 20)).await;
        store
            .record_bot_event_outcomes(
                &alpha,
                &["e-1".to_owned()],
                outcome(BotEventOutcome::Handled, T0 + 60),
            )
            .await
            .unwrap();

        let roster = store.list_bot_roster().await.unwrap();
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].bot.bot_id, alpha);
        assert_eq!(roster[0].trigger_count, 2);
        assert_eq!(roster[0].pending_count, 2);
        assert_eq!(
            roster[0].last_event.as_ref().map(|event| event.seq),
            Some(3)
        );
        assert_eq!(roster[1].bot.bot_id, bot("beta"));
        assert_eq!(roster[1].trigger_count, 0);
        assert_eq!(roster[1].pending_count, 0);
        assert_eq!(roster[1].last_event, None);

        assert!(store.delete_bot_event(&alpha, "e-3").await.unwrap());
        let roster = store.list_bot_roster().await.unwrap();
        assert_eq!(
            roster[0].last_event.as_ref().map(|event| event.seq),
            Some(2)
        );
        assert_eq!(roster[0].pending_count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_bot_cascades_triggers_and_events() {
        let store = store_with(&["alpha", "beta"]).await;
        let alpha = bot("alpha");
        let beta = bot("beta");
        store
            .put_bot_trigger(&alpha, write("hourly", schedule()), None, T0)
            .await
            .unwrap();
        store
            .put_bot_trigger(&beta, write("hourly", schedule()), None, T0)
            .await
            .unwrap();
        insert(&store, event(&alpha, "e-1", 1, T0)).await;
        insert(&store, event(&beta, "e-1", 1, T0)).await;

        let removed = store.delete_bot(&alpha).await.unwrap();
        assert_eq!(removed.bot_id, alpha);
        assert!(matches!(
            store.read_bot(&alpha).await.unwrap_err(),
            BotError::BotNotFound { .. }
        ));
        assert!(store.list_bot_triggers(&alpha).await.unwrap().is_empty());
        assert!(
            store
                .list_bot_events(&alpha, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            store.read_bot_event_by_seq(&alpha, 1).await.unwrap_err(),
            BotError::EventNotFound { .. }
        ));
        assert_eq!(
            ids(&store
                .list_bot_triggers_by_kind(BotTriggerKind::Schedule)
                .await
                .unwrap()),
            vec![("beta".to_owned(), "hourly".to_owned())]
        );
        assert_eq!(store.read_bot_event_by_seq(&beta, 1).await.unwrap().seq, 1);
        assert!(matches!(
            store.delete_bot(&alpha).await.unwrap_err(),
            BotError::BotNotFound { .. }
        ));
    }

    // ── Triggers ────────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn put_bot_trigger_creates_then_replaces() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        let mut first = write("hook", webhook());
        first.secrets.webhook_token = Some("tok-1".to_owned());
        let created = store
            .put_bot_trigger(&triage, first, None, T0)
            .await
            .unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(created.kind(), BotTriggerKind::Webhook);
        assert_eq!(created.secrets.webhook_token.as_deref(), Some("tok-1"));
        assert_eq!(created.created_at_ms, T0);
        assert_eq!(created.updated_at_ms, T0);
        assert_eq!(created.cursor, None);
        assert_eq!(created.disabled_reason, None);
        assert_eq!(
            store
                .read_bot_trigger(&triage, &trigger("hook"))
                .await
                .unwrap(),
            created
        );

        let mut second = write("hook", webhook());
        second.document.filter = Some("event.kind == 'push'".to_owned());
        second.secrets.webhook_token = Some("tok-2".to_owned());
        let replaced = store
            .put_bot_trigger(&triage, second.clone(), Some(1), T0 + 10)
            .await
            .unwrap();
        assert_eq!(replaced.revision, 2);
        assert_eq!(replaced.document, second.document);
        assert_eq!(replaced.secrets.webhook_token.as_deref(), Some("tok-2"));
        assert_eq!(replaced.created_at_ms, T0);
        assert_eq!(replaced.updated_at_ms, T0 + 10);

        let error = store
            .put_bot_trigger(&triage, second, Some(1), T0 + 20)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            BotError::TriggerRevisionConflict {
                bot_id: triage.clone(),
                trigger_id: trigger("hook"),
                expected: 1,
                actual: 2,
            }
        );
        assert_eq!(
            store
                .read_bot_trigger(&triage, &trigger("hook"))
                .await
                .unwrap()
                .revision,
            2
        );

        // Expected revision only matters when the trigger exists.
        let fresh = store
            .put_bot_trigger(&triage, write("fresh", inbox()), Some(7), T0)
            .await
            .unwrap();
        assert_eq!(fresh.revision, 1);

        assert_eq!(
            store
                .put_bot_trigger(&bot("missing"), write("hook", webhook()), None, T0)
                .await
                .unwrap_err(),
            BotError::BotNotFound {
                bot_id: bot("missing")
            }
        );
        assert_eq!(
            store
                .read_bot_trigger(&triage, &trigger("nope"))
                .await
                .unwrap_err(),
            BotError::TriggerNotFound {
                bot_id: triage.clone(),
                trigger_id: trigger("nope"),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_bot_trigger_validates_the_document() {
        let store = store_with(&["triage"]).await;
        let mut invalid = schedule();
        invalid.filter = Some("true".to_owned()); // schedules take no filter
        assert!(matches!(
            store
                .put_bot_trigger(&bot("triage"), write("hourly", invalid), None, T0)
                .await
                .unwrap_err(),
            BotError::InvalidInput { .. }
        ));
        assert!(
            store
                .list_bot_triggers(&bot("triage"))
                .await
                .unwrap()
                .is_empty()
        );

        // The one-shot lead check is anchored at `now_ms`.
        let one_shot = trigger_document(BotTriggerSpec::Schedule {
            cron: None,
            at_ms: Some(T0 + 10_000),
            timezone: "UTC".to_owned(),
            summary: "once".to_owned(),
        });
        assert!(
            store
                .put_bot_trigger(&bot("triage"), write("once", one_shot.clone()), None, T0)
                .await
                .is_err()
        );
        assert!(
            store
                .put_bot_trigger(&bot("triage"), write("once", one_shot), None, T0 - 60_000)
                .await
                .is_ok()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_bot_trigger_cursor_override() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        let mut created = write("feed", poll());
        created.cursor = Some(Some(cursor_state(&["a"])));
        let record = store
            .put_bot_trigger(&triage, created, None, T0)
            .await
            .unwrap();
        assert_eq!(record.cursor, Some(cursor_state(&["a"])));

        // `None` leaves the stored cursor alone.
        let record = store
            .put_bot_trigger(&triage, write("feed", poll()), None, T0 + 1)
            .await
            .unwrap();
        assert_eq!(record.cursor, Some(cursor_state(&["a"])));

        // `Some(Some(_))` replaces it.
        let mut replace = write("feed", poll());
        replace.cursor = Some(Some(cursor_state(&["b", "c"])));
        let record = store
            .put_bot_trigger(&triage, replace, None, T0 + 2)
            .await
            .unwrap();
        assert_eq!(record.cursor, Some(cursor_state(&["b", "c"])));

        // `Some(None)` clears it (a spec edit).
        let mut clear = write("feed", poll());
        clear.cursor = Some(None);
        let record = store
            .put_bot_trigger(&triage, clear, None, T0 + 3)
            .await
            .unwrap();
        assert_eq!(record.cursor, None);
        assert_eq!(record.revision, 4);

        store
            .set_bot_trigger_cursor(&triage, &trigger("feed"), Some(cursor_state(&["z"])))
            .await
            .unwrap();
        let record = store
            .read_bot_trigger(&triage, &trigger("feed"))
            .await
            .unwrap();
        assert_eq!(record.cursor, Some(cursor_state(&["z"])));
        assert_eq!(record.revision, 4, "cursor writes are not document changes");
        store
            .set_bot_trigger_cursor(&triage, &trigger("feed"), None)
            .await
            .unwrap();
        assert_eq!(
            store
                .read_bot_trigger(&triage, &trigger("feed"))
                .await
                .unwrap()
                .cursor,
            None
        );
        assert!(matches!(
            store
                .set_bot_trigger_cursor(&triage, &trigger("nope"), None)
                .await
                .unwrap_err(),
            BotError::TriggerNotFound { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disable_bot_trigger_sets_reason_and_document_put_re_enables() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        store
            .put_bot_trigger(&triage, write("hook", webhook()), None, T0)
            .await
            .unwrap();
        store
            .set_bot_trigger_filter_error(
                &triage,
                &trigger("hook"),
                Some("no such field".to_owned()),
                T0 + 1,
            )
            .await
            .unwrap();

        let disabled = store
            .disable_bot_trigger(
                &triage,
                &trigger("hook"),
                BotTriggerDisabledReason::Breaker,
                T0 + 5,
            )
            .await
            .unwrap();
        assert!(!disabled.enabled());
        assert_eq!(
            disabled.disabled_reason,
            Some(BotTriggerDisabledReason::Breaker)
        );
        assert_eq!(disabled.disabled_at_ms, Some(T0 + 5));
        assert_eq!(disabled.updated_at_ms, T0 + 5);
        assert_eq!(disabled.revision, 2);
        assert_eq!(
            store
                .read_bot_trigger(&triage, &trigger("hook"))
                .await
                .unwrap(),
            disabled
        );

        // A put that keeps it disabled keeps the incident.
        let mut still_off = write("hook", webhook());
        still_off.document.enabled = false;
        let record = store
            .put_bot_trigger(&triage, still_off, Some(2), T0 + 6)
            .await
            .unwrap();
        assert_eq!(
            record.disabled_reason,
            Some(BotTriggerDisabledReason::Breaker)
        );
        assert_eq!(record.disabled_at_ms, Some(T0 + 5));
        assert_eq!(record.last_filter_error.as_deref(), Some("no such field"));

        // Enabling through the document clears the disable, not the filter
        // incident.
        let record = store
            .put_bot_trigger(&triage, write("hook", webhook()), Some(3), T0 + 7)
            .await
            .unwrap();
        assert!(record.enabled());
        assert_eq!(record.disabled_reason, None);
        assert_eq!(record.disabled_at_ms, None);
        assert_eq!(record.last_filter_error.as_deref(), Some("no such field"));
        assert_eq!(record.last_filter_error_at_ms, Some(T0 + 1));

        assert!(matches!(
            store
                .disable_bot_trigger(
                    &triage,
                    &trigger("nope"),
                    BotTriggerDisabledReason::Operator,
                    T0
                )
                .await
                .unwrap_err(),
            BotError::TriggerNotFound { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disable_bot_triggers_returns_only_the_changed_ones() {
        let store = store_with(&["triage", "other"]).await;
        let triage = bot("triage");
        store
            .put_bot_trigger(&triage, write("hourly", schedule()), None, T0)
            .await
            .unwrap();
        store
            .put_bot_trigger(&triage, write("hook", webhook()), None, T0)
            .await
            .unwrap();
        let mut off = write("inbox", inbox());
        off.document.enabled = false;
        store.put_bot_trigger(&triage, off, None, T0).await.unwrap();
        store
            .put_bot_trigger(&bot("other"), write("hook", webhook()), None, T0)
            .await
            .unwrap();

        let changed = store
            .disable_bot_triggers(&triage, BotTriggerDisabledReason::BotClosed, T0 + 5)
            .await
            .unwrap();
        assert_eq!(
            ids(&changed),
            vec![
                ("triage".to_owned(), "hook".to_owned()),
                ("triage".to_owned(), "hourly".to_owned()),
            ]
        );
        for record in &changed {
            assert!(!record.enabled());
            assert_eq!(
                record.disabled_reason,
                Some(BotTriggerDisabledReason::BotClosed)
            );
            assert_eq!(record.disabled_at_ms, Some(T0 + 5));
            assert_eq!(record.revision, 2);
        }
        let untouched = store
            .read_bot_trigger(&triage, &trigger("inbox"))
            .await
            .unwrap();
        assert_eq!(untouched.disabled_reason, None);
        assert_eq!(untouched.revision, 1);
        assert!(
            store
                .read_bot_trigger(&bot("other"), &trigger("hook"))
                .await
                .unwrap()
                .enabled()
        );

        let again = store
            .disable_bot_triggers(&triage, BotTriggerDisabledReason::BotClosed, T0 + 6)
            .await
            .unwrap();
        assert!(again.is_empty());
        assert!(
            store
                .disable_bot_triggers(&bot("missing"), BotTriggerDisabledReason::BotClosed, T0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_bot_triggers_orders_by_trigger_id_and_by_kind_across_bots() {
        let store = store_with(&["b-beta", "b-alpha"]).await;
        for (owner, name, document) in [
            ("b-beta", "z-hourly", schedule()),
            ("b-beta", "a-daily", schedule()),
            ("b-beta", "hook", webhook()),
            ("b-alpha", "nightly", schedule()),
            ("b-alpha", "feed", poll()),
        ] {
            store
                .put_bot_trigger(&bot(owner), write(name, document), None, T0)
                .await
                .unwrap();
        }
        assert_eq!(
            ids(&store.list_bot_triggers(&bot("b-beta")).await.unwrap()),
            vec![
                ("b-beta".to_owned(), "a-daily".to_owned()),
                ("b-beta".to_owned(), "hook".to_owned()),
                ("b-beta".to_owned(), "z-hourly".to_owned()),
            ]
        );
        assert_eq!(
            ids(&store
                .list_bot_triggers_by_kind(BotTriggerKind::Schedule)
                .await
                .unwrap()),
            vec![
                ("b-alpha".to_owned(), "nightly".to_owned()),
                ("b-beta".to_owned(), "a-daily".to_owned()),
                ("b-beta".to_owned(), "z-hourly".to_owned()),
            ]
        );
        assert_eq!(
            ids(&store
                .list_bot_triggers_by_kind(BotTriggerKind::Poll)
                .await
                .unwrap()),
            vec![("b-alpha".to_owned(), "feed".to_owned())]
        );
        assert!(
            store
                .list_bot_triggers_by_kind(BotTriggerKind::Chat)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_bot_triggers(&bot("missing"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn filter_error_is_set_and_cleared_without_touching_the_document() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        store
            .put_bot_trigger(&triage, write("hook", webhook()), None, T0)
            .await
            .unwrap();
        store
            .set_bot_trigger_filter_error(
                &triage,
                &trigger("hook"),
                Some("boom".to_owned()),
                T0 + 3,
            )
            .await
            .unwrap();
        let record = store
            .read_bot_trigger(&triage, &trigger("hook"))
            .await
            .unwrap();
        assert_eq!(record.last_filter_error.as_deref(), Some("boom"));
        assert_eq!(record.last_filter_error_at_ms, Some(T0 + 3));
        assert_eq!(record.revision, 1);
        assert_eq!(record.updated_at_ms, T0);

        store
            .set_bot_trigger_filter_error(&triage, &trigger("hook"), None, T0 + 4)
            .await
            .unwrap();
        let record = store
            .read_bot_trigger(&triage, &trigger("hook"))
            .await
            .unwrap();
        assert_eq!(record.last_filter_error, None);
        assert_eq!(record.last_filter_error_at_ms, None);
        assert!(matches!(
            store
                .set_bot_trigger_filter_error(&triage, &trigger("nope"), None, T0)
                .await
                .unwrap_err(),
            BotError::TriggerNotFound { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_bot_trigger_keeps_the_event_log() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        store
            .put_bot_trigger(&triage, write("hook", webhook()), None, T0)
            .await
            .unwrap();
        let mut from_hook = event(&triage, "e-1", 1, T0);
        from_hook.trigger_id = Some(trigger("hook"));
        insert(&store, from_hook).await;

        let removed = store
            .delete_bot_trigger(&triage, &trigger("hook"))
            .await
            .unwrap();
        assert_eq!(removed.trigger_id, trigger("hook"));
        assert_eq!(
            store
                .delete_bot_trigger(&triage, &trigger("hook"))
                .await
                .unwrap_err(),
            BotError::TriggerNotFound {
                bot_id: triage.clone(),
                trigger_id: trigger("hook"),
            }
        );
        let kept = store.read_bot_event_by_seq(&triage, 1).await.unwrap();
        assert_eq!(kept.trigger_id, Some(trigger("hook")));
    }

    // ── Events ──────────────────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn insert_bot_event_returns_the_stored_row_on_duplicate_id() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        let first = insert(&store, event(&triage, "e-1", 1, T0)).await;

        let mut retry = event(&triage, "e-1", 2, T0 + 1);
        retry.document_ref = "doc-retry".to_owned();
        let outcome = store.insert_bot_event(retry).await.unwrap();
        assert!(outcome.is_duplicate());
        assert_eq!(outcome, InsertBotEventOutcome::Duplicate(first.clone()));
        assert_eq!(outcome.record().seq, 1);
        assert!(matches!(
            store.read_bot_event_by_seq(&triage, 2).await.unwrap_err(),
            BotError::EventNotFound { seq: 2, .. }
        ));

        // A different id with the same bot is fine; the same seq is not.
        insert(&store, event(&triage, "e-2", 2, T0)).await;
        assert!(matches!(
            store
                .insert_bot_event(event(&triage, "e-3", 2, T0))
                .await
                .unwrap_err(),
            BotError::InvalidInput { .. }
        ));
        assert!(matches!(
            store
                .insert_bot_event(event(&bot("missing"), "e-1", 1, T0))
                .await
                .unwrap_err(),
            BotError::BotNotFound { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_bot_event_reports_whether_a_row_went() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        insert(&store, event(&triage, "e-1", 1, T0)).await;
        assert!(store.delete_bot_event(&triage, "e-1").await.unwrap());
        assert!(!store.delete_bot_event(&triage, "e-1").await.unwrap());
        assert!(matches!(
            store.read_bot_event(&triage, "e-1").await.unwrap_err(),
            BotError::EventIdNotFound { .. }
        ));
        assert!(matches!(
            store.read_bot_event_by_seq(&triage, 1).await.unwrap_err(),
            BotError::EventNotFound { .. }
        ));
        // The seq is free again after compensation.
        insert(&store, event(&triage, "e-1b", 1, T0)).await;
        assert_eq!(
            store
                .read_bot_event_by_seq(&triage, 1)
                .await
                .unwrap()
                .event_id,
            "e-1b"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_bot_events_by_seq_by_id_and_in_batch() {
        let store = store_with(&["triage", "other"]).await;
        let triage = bot("triage");
        for (id, seq) in [("e-3", 3), ("e-1", 1), ("e-2", 2)] {
            insert(&store, event(&triage, id, seq, T0)).await;
        }
        insert(&store, event(&bot("other"), "e-1", 1, T0)).await;

        assert_eq!(
            store
                .read_bot_event_by_seq(&triage, 2)
                .await
                .unwrap()
                .event_id,
            "e-2"
        );
        assert_eq!(store.read_bot_event(&triage, "e-3").await.unwrap().seq, 3);
        assert_eq!(
            store.read_bot_event(&triage, "nope").await.unwrap_err(),
            BotError::EventIdNotFound {
                bot_id: triage.clone(),
                event_id: "nope".to_owned(),
            }
        );
        assert_eq!(
            store.read_bot_event_by_seq(&triage, 9).await.unwrap_err(),
            BotError::EventNotFound {
                bot_id: triage.clone(),
                seq: 9,
            }
        );

        let batch = store
            .read_bot_events(
                &triage,
                &[
                    "e-3".to_owned(),
                    "missing".to_owned(),
                    "e-1".to_owned(),
                    "e-3".to_owned(),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            seqs(&batch),
            vec![1, 3],
            "log order, once each, unknown skipped"
        );
        assert!(batch.iter().all(|record| record.bot_id == triage));
        assert!(
            store
                .read_bot_events(&triage, &[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_bot_events_pages_newest_first_by_receipt_then_seq() {
        let store = store_with(&["triage", "other"]).await;
        let triage = bot("triage");
        // Two receipts share a timestamp; #4 arrived with an older clock.
        for (id, seq, at) in [
            ("e-1", 1, T0 + 10),
            ("e-2", 2, T0 + 20),
            ("e-3", 3, T0 + 20),
            ("e-4", 4, T0 + 5),
            ("e-5", 5, T0 + 30),
        ] {
            insert(&store, event(&triage, id, seq, at)).await;
        }
        insert(&store, event(&bot("other"), "e-9", 9, T0 + 99)).await;

        let all = store.list_bot_events(&triage, 10, None).await.unwrap();
        assert_eq!(seqs(&all), vec![5, 3, 2, 1, 4]);

        let page = store.list_bot_events(&triage, 2, None).await.unwrap();
        assert_eq!(seqs(&page), vec![5, 3]);
        let last = page.last().unwrap();
        let cursor = BotEventCursor {
            received_at_ms: last.received_at_ms,
            seq: last.seq,
        };
        let page = store
            .list_bot_events(&triage, 2, Some(cursor))
            .await
            .unwrap();
        assert_eq!(
            seqs(&page),
            vec![2, 1],
            "the tie on received_at_ms continues by seq"
        );
        let last = page.last().unwrap();
        let cursor = BotEventCursor {
            received_at_ms: last.received_at_ms,
            seq: last.seq,
        };
        let page = store
            .list_bot_events(&triage, 2, Some(cursor))
            .await
            .unwrap();
        assert_eq!(seqs(&page), vec![4]);
        let last = page.last().unwrap();
        let cursor = BotEventCursor {
            received_at_ms: last.received_at_ms,
            seq: last.seq,
        };
        assert!(
            store
                .list_bot_events(&triage, 2, Some(cursor))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_bot_events(&triage, 0, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_bot_events(&bot("missing"), 5, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn count_bot_events_since_by_trigger_and_by_sender() {
        let store = store_with(&["triage", "other", "sender"]).await;
        let triage = bot("triage");
        let other = bot("other");
        let sender = bot("sender");
        let mut seq = 0;
        let mut next = |owner: &BotId, at: i64, via: Option<&str>, from: Option<&BotId>| {
            seq += 1;
            let mut record = event(owner, &format!("e-{seq}"), seq, at);
            record.trigger_id = via.map(trigger);
            record.sender_bot_id = from.cloned();
            record
        };
        for record in [
            next(&triage, T0 + 10, Some("hook"), None),
            next(&triage, T0 + 20, Some("hook"), Some(&sender)),
            next(&triage, T0 + 30, Some("inbox"), Some(&sender)),
            next(&triage, T0 + 5, Some("hook"), None),
            next(&other, T0 + 40, Some("hook"), Some(&sender)),
            next(&other, T0 + 50, Some("hook"), Some(&triage)),
        ] {
            insert(&store, record).await;
        }

        let hook = trigger("hook");
        let by_trigger = |bot_id, trigger_id, since_ms| {
            store
                .count_bot_events_since(BotEventRateScope::Trigger { bot_id, trigger_id }, since_ms)
        };
        assert_eq!(by_trigger(&triage, &hook, T0).await.unwrap(), 3);
        assert_eq!(
            by_trigger(&triage, &hook, T0 + 10).await.unwrap(),
            2,
            "inclusive"
        );
        assert_eq!(by_trigger(&triage, &hook, T0 + 11).await.unwrap(), 1);
        assert_eq!(by_trigger(&triage, &hook, T0 + 100).await.unwrap(), 0);
        assert_eq!(by_trigger(&other, &hook, T0).await.unwrap(), 2);
        assert_eq!(by_trigger(&triage, &trigger("nope"), T0).await.unwrap(), 0);

        let by_sender = |sender_bot_id, since_ms| {
            store.count_bot_events_since(BotEventRateScope::Sender { sender_bot_id }, since_ms)
        };
        assert_eq!(
            by_sender(&sender, T0).await.unwrap(),
            3,
            "across receiving bots"
        );
        assert_eq!(by_sender(&sender, T0 + 40).await.unwrap(), 1);
        assert_eq!(by_sender(&triage, T0).await.unwrap(), 1);
        assert_eq!(by_sender(&other, T0).await.unwrap(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_bot_event_outcomes_is_write_once() {
        let store = store_with(&["triage"]).await;
        let triage = bot("triage");
        for (id, seq) in [("e-1", 1), ("e-2", 2), ("e-3", 3)] {
            insert(&store, event(&triage, id, seq, T0)).await;
        }
        let ids = ["e-1".to_owned(), "e-2".to_owned(), "missing".to_owned()];
        let changed = store
            .record_bot_event_outcomes(&triage, &ids, outcome(BotEventOutcome::Handled, T0 + 9))
            .await
            .unwrap();
        assert_eq!(changed, 2);
        let record = store.read_bot_event(&triage, "e-1").await.unwrap();
        assert_eq!(record.outcome, Some(BotEventOutcome::Handled));
        assert_eq!(record.outcome_detail.as_deref(), Some("done"));
        assert_eq!(record.run_id.as_deref(), Some("run-1"));
        assert_eq!(record.resolved_at_ms, Some(T0 + 9));
        assert!(!record.is_pending());
        assert!(
            store
                .read_bot_event(&triage, "e-3")
                .await
                .unwrap()
                .is_pending()
        );

        // Already-resolved rows are left alone, pending ones in the same
        // call are written.
        let ids = ["e-1".to_owned(), "e-3".to_owned(), "e-3".to_owned()];
        let changed = store
            .record_bot_event_outcomes(&triage, &ids, outcome(BotEventOutcome::RunFailed, T0 + 20))
            .await
            .unwrap();
        assert_eq!(changed, 1);
        let record = store.read_bot_event(&triage, "e-1").await.unwrap();
        assert_eq!(record.outcome, Some(BotEventOutcome::Handled));
        assert_eq!(record.resolved_at_ms, Some(T0 + 9));
        let record = store.read_bot_event(&triage, "e-3").await.unwrap();
        assert_eq!(record.outcome, Some(BotEventOutcome::RunFailed));
        assert_eq!(record.resolved_at_ms, Some(T0 + 20));

        let changed = store
            .record_bot_event_outcomes(&triage, &ids, outcome(BotEventOutcome::Archived, T0 + 30))
            .await
            .unwrap();
        assert_eq!(changed, 0);
        assert_eq!(store.list_bot_roster().await.unwrap()[0].pending_count, 0);
        assert_eq!(
            store
                .record_bot_event_outcomes(
                    &bot("missing"),
                    &ids,
                    outcome(BotEventOutcome::Archived, T0)
                )
                .await
                .unwrap(),
            0
        );
    }
}
