//! Session event-log storage contract.

use crate::session::{
    DynamicSessionEntry, DynamicUncommittedSessionEvent, EventSeq, SessionId, SessionPosition,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub head: Option<SessionPosition>,
    pub source_session_id: Option<SessionId>,
    pub source_seq: Option<EventSeq>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSession {
    pub session_id: SessionId,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateClonedSession {
    pub source_session_id: SessionId,
    pub session_id: SessionId,
    pub created_at_ms: u64,
    #[serde(default)]
    pub opening_events: Vec<DynamicUncommittedSessionEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateForkedSession {
    pub source_session_id: SessionId,
    pub session_id: SessionId,
    /// Branch point in the source session's effective log. `0` means an empty
    /// inherited prefix; the child then appends from seq 1.
    pub source_seq: EventSeq,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendSessionEvents {
    pub session_id: SessionId,
    pub expected_head: Option<SessionPosition>,
    pub events: Vec<DynamicUncommittedSessionEvent>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendSessionEventsResult {
    pub entries: Vec<DynamicSessionEntry>,
    pub head: Option<SessionPosition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSessionEvents {
    pub session_id: SessionId,
    pub after: Option<EventSeq>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertSessionLink {
    pub from_session_id: SessionId,
    pub to_session_id: SessionId,
    pub relationship: String,
    pub created_at_ms: u64,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLinkDirection {
    #[default]
    Outgoing,
    Incoming,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSessionLinks {
    pub session_id: SessionId,
    #[serde(default)]
    pub direction: SessionLinkDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLinkRecord {
    pub from_session_id: SessionId,
    pub to_session_id: SessionId,
    pub relationship: String,
    pub created_at_ms: u64,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPage {
    pub entries: Vec<DynamicSessionEntry>,
    pub next_after: Option<EventSeq>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionStoreError {
    #[error("session already exists: {session_id}")]
    SessionAlreadyExists { session_id: SessionId },

    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: SessionId },

    #[error("expected head mismatch for {session_id}: expected {expected:?}, actual {actual:?}")]
    ExpectedHeadMismatch {
        session_id: SessionId,
        expected: Option<SessionPosition>,
        actual: Option<SessionPosition>,
    },

    #[error("invalid page limit: {limit}")]
    InvalidLimit { limit: usize },

    #[error("invalid fork point for {session_id} at seq {source_seq}: {message}")]
    InvalidForkPoint {
        session_id: SessionId,
        source_seq: EventSeq,
        message: String,
    },

    #[error("invalid session link relationship: {relationship:?}")]
    InvalidRelationship { relationship: String },

    #[error("session store failure: {message}")]
    Store { message: String },
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError>;

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError>;

    async fn create_cloned_session(
        &self,
        request: CreateClonedSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        Err(SessionStoreError::Store {
            message: format!(
                "create_cloned_session is not supported by this session store for {}",
                request.session_id
            ),
        })
    }

    async fn create_forked_session(
        &self,
        request: CreateForkedSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        Err(SessionStoreError::Store {
            message: format!(
                "create_forked_session is not supported by this session store for {}",
                request.session_id
            ),
        })
    }

    async fn safe_fork_seq(&self, session_id: &SessionId) -> Result<EventSeq, SessionStoreError> {
        Err(SessionStoreError::Store {
            message: format!(
                "safe_fork_seq is not supported by this session store for {session_id}"
            ),
        })
    }

    async fn upsert_link(
        &self,
        request: UpsertSessionLink,
    ) -> Result<SessionLinkRecord, SessionStoreError> {
        Err(SessionStoreError::Store {
            message: format!(
                "upsert_link is not supported by this session store for {} -> {}",
                request.from_session_id, request.to_session_id
            ),
        })
    }

    async fn list_links(
        &self,
        request: ListSessionLinks,
    ) -> Result<Vec<SessionLinkRecord>, SessionStoreError> {
        Err(SessionStoreError::Store {
            message: format!(
                "list_links is not supported by this session store for {}",
                request.session_id
            ),
        })
    }

    async fn append(
        &self,
        request: AppendSessionEvents,
    ) -> Result<AppendSessionEventsResult, SessionStoreError>;

    async fn read_after(
        &self,
        request: ReadSessionEvents,
    ) -> Result<SessionPage, SessionStoreError>;

    async fn head(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionPosition>, SessionStoreError>;
}

#[derive(Clone, Default)]
pub struct InMemorySessionStore {
    inner: Arc<RwLock<InMemorySessionStoreInner>>,
}

#[derive(Default)]
struct InMemorySessionStoreInner {
    records: BTreeMap<SessionId, SessionRecord>,
    entries: BTreeMap<SessionId, Vec<DynamicSessionEntry>>,
    links: BTreeMap<(SessionId, SessionId, String), SessionLinkRecord>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut inner = self.inner.write().map_err(|_| SessionStoreError::Store {
            message: "session store write lock poisoned".into(),
        })?;
        if inner.records.contains_key(&request.session_id) {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        }
        let record = SessionRecord {
            session_id: request.session_id,
            head: None,
            source_session_id: None,
            source_seq: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        inner.entries.insert(record.session_id.clone(), Vec::new());
        inner
            .records
            .insert(record.session_id.clone(), record.clone());
        Ok(record)
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let inner = self.inner.read().map_err(|_| SessionStoreError::Store {
            message: "session store read lock poisoned".into(),
        })?;
        Ok(inner.records.get(session_id).cloned())
    }

    async fn create_cloned_session(
        &self,
        request: CreateClonedSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut inner = self.inner.write().map_err(|_| SessionStoreError::Store {
            message: "session store write lock poisoned".into(),
        })?;
        if !inner.records.contains_key(&request.source_session_id) {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.source_session_id,
            });
        }
        if inner.records.contains_key(&request.session_id) {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        }

        let mut record = SessionRecord {
            session_id: request.session_id,
            head: None,
            source_session_id: Some(request.source_session_id),
            source_seq: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        let committed = commit_uncommitted_events(&mut record, request.opening_events);
        inner.entries.insert(record.session_id.clone(), committed);
        inner
            .records
            .insert(record.session_id.clone(), record.clone());
        Ok(record)
    }

    async fn create_forked_session(
        &self,
        request: CreateForkedSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        let mut inner = self.inner.write().map_err(|_| SessionStoreError::Store {
            message: "session store write lock poisoned".into(),
        })?;
        if !inner.records.contains_key(&request.source_session_id) {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.source_session_id,
            });
        }
        if inner.records.contains_key(&request.session_id) {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        }
        validate_in_memory_fork_point(&inner, &request.source_session_id, request.source_seq)?;
        let head = position_from_nonzero_seq(request.source_seq);
        let record = SessionRecord {
            session_id: request.session_id,
            head,
            source_session_id: Some(request.source_session_id),
            source_seq: Some(request.source_seq),
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        inner.entries.insert(record.session_id.clone(), Vec::new());
        inner
            .records
            .insert(record.session_id.clone(), record.clone());
        Ok(record)
    }

    async fn safe_fork_seq(&self, session_id: &SessionId) -> Result<EventSeq, SessionStoreError> {
        let inner = self.inner.read().map_err(|_| SessionStoreError::Store {
            message: "session store read lock poisoned".into(),
        })?;
        let entries = effective_entries(&inner, session_id)?;
        Ok(largest_safe_fork_seq(
            &entries,
            effective_head_u64(&inner, session_id)?,
        ))
    }

    async fn upsert_link(
        &self,
        request: UpsertSessionLink,
    ) -> Result<SessionLinkRecord, SessionStoreError> {
        validate_relationship(&request.relationship)?;
        let mut inner = self.inner.write().map_err(|_| SessionStoreError::Store {
            message: "session store write lock poisoned".into(),
        })?;
        for session_id in [&request.from_session_id, &request.to_session_id] {
            if !inner.records.contains_key(session_id) {
                return Err(SessionStoreError::SessionNotFound {
                    session_id: session_id.clone(),
                });
            }
        }
        let record = SessionLinkRecord {
            from_session_id: request.from_session_id,
            to_session_id: request.to_session_id,
            relationship: request.relationship,
            created_at_ms: request.created_at_ms,
            metadata: request.metadata,
        };
        inner.links.insert(
            (
                record.from_session_id.clone(),
                record.to_session_id.clone(),
                record.relationship.clone(),
            ),
            record.clone(),
        );
        Ok(record)
    }

    async fn list_links(
        &self,
        request: ListSessionLinks,
    ) -> Result<Vec<SessionLinkRecord>, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        let inner = self.inner.read().map_err(|_| SessionStoreError::Store {
            message: "session store read lock poisoned".into(),
        })?;
        if !inner.records.contains_key(&request.session_id) {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.session_id,
            });
        }
        Ok(inner
            .links
            .values()
            .filter(|record| match request.direction {
                SessionLinkDirection::Outgoing => record.from_session_id == request.session_id,
                SessionLinkDirection::Incoming => record.to_session_id == request.session_id,
            })
            .filter(|record| {
                request
                    .relationship
                    .as_ref()
                    .is_none_or(|relationship| &record.relationship == relationship)
            })
            .take(request.limit)
            .cloned()
            .collect())
    }

    async fn append(
        &self,
        request: AppendSessionEvents,
    ) -> Result<AppendSessionEventsResult, SessionStoreError> {
        let mut inner = self.inner.write().map_err(|_| SessionStoreError::Store {
            message: "session store write lock poisoned".into(),
        })?;
        let actual_head = inner
            .records
            .get(&request.session_id)
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: request.session_id.clone(),
            })?
            .head
            .clone();
        if request.expected_head != actual_head {
            return Err(SessionStoreError::ExpectedHeadMismatch {
                session_id: request.session_id,
                expected: request.expected_head,
                actual: actual_head,
            });
        }

        let record = inner
            .records
            .get_mut(&request.session_id)
            .expect("validated session record");
        let committed = commit_uncommitted_events(record, request.events);
        let head = record.head.clone();

        let entries = inner
            .entries
            .get_mut(&request.session_id)
            .expect("session entries exist for record");
        entries.extend(committed.clone());

        Ok(AppendSessionEventsResult {
            entries: committed,
            head,
        })
    }

    async fn read_after(
        &self,
        request: ReadSessionEvents,
    ) -> Result<SessionPage, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        let inner = self.inner.read().map_err(|_| SessionStoreError::Store {
            message: "session store read lock poisoned".into(),
        })?;
        let entries = effective_entries(&inner, &request.session_id)?;
        let mut selected = entries
            .iter()
            .filter(|entry| request.after.is_none_or(|after| entry.position.seq > after))
            .take(request.limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        let complete = selected.len() <= request.limit;
        if !complete {
            selected.truncate(request.limit);
        }
        let next_after = selected.last().map(|entry| entry.position.seq);
        Ok(SessionPage {
            entries: selected,
            next_after,
            complete,
        })
    }

    async fn head(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionPosition>, SessionStoreError> {
        let inner = self.inner.read().map_err(|_| SessionStoreError::Store {
            message: "session store read lock poisoned".into(),
        })?;
        Ok(inner
            .records
            .get(session_id)
            .and_then(|record| record.head.clone()))
    }
}

pub fn validate_relationship(relationship: &str) -> Result<(), SessionStoreError> {
    if relationship.is_empty() {
        return Err(SessionStoreError::InvalidRelationship {
            relationship: relationship.to_owned(),
        });
    }
    Ok(())
}

pub fn largest_safe_fork_seq(entries: &[DynamicSessionEntry], head: u64) -> EventSeq {
    let ranges = core_run_ranges(entries);
    let earliest_open = ranges
        .values()
        .filter(|range| range.terminal_seq.is_none())
        .map(|range| range.first_seq)
        .min();
    EventSeq::new(earliest_open.map_or(head, |seq| seq.saturating_sub(1)))
}

pub fn validate_fork_point(
    session_id: &SessionId,
    source_seq: EventSeq,
    entries: &[DynamicSessionEntry],
    head: u64,
) -> Result<(), SessionStoreError> {
    let source_seq_u64 = source_seq.as_u64();
    if source_seq_u64 > head {
        return Err(SessionStoreError::InvalidForkPoint {
            session_id: session_id.clone(),
            source_seq,
            message: format!("source seq is beyond session head {head}"),
        });
    }
    for range in core_run_ranges(entries).values() {
        let end_exclusive = range.terminal_seq.map_or(head.saturating_add(1), |seq| seq);
        if source_seq_u64 >= range.first_seq && source_seq_u64 < end_exclusive {
            return Err(SessionStoreError::InvalidForkPoint {
                session_id: session_id.clone(),
                source_seq,
                message: format!(
                    "seq is inside non-terminal run {} ({}..{})",
                    range.run_id,
                    range.first_seq,
                    range
                        .terminal_seq
                        .map_or_else(|| "head".to_owned(), |seq| seq.to_string())
                ),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct RunRange {
    run_id: u64,
    first_seq: u64,
    terminal_seq: Option<u64>,
}

fn core_run_ranges(entries: &[DynamicSessionEntry]) -> BTreeMap<u64, RunRange> {
    let mut ranges = BTreeMap::new();
    for entry in entries {
        let Some(boundary) = run_boundary(entry) else {
            continue;
        };
        let range = ranges.entry(boundary.run_id).or_insert_with(|| RunRange {
            run_id: boundary.run_id,
            first_seq: entry.position.seq.as_u64(),
            terminal_seq: None,
        });
        range.first_seq = range.first_seq.min(entry.position.seq.as_u64());
        if boundary.terminal {
            range.terminal_seq = Some(entry.position.seq.as_u64());
        }
    }
    ranges
}

#[derive(Clone, Copy, Debug)]
struct RunBoundary {
    run_id: u64,
    terminal: bool,
}

fn run_boundary(entry: &DynamicSessionEntry) -> Option<RunBoundary> {
    let run_id = entry
        .joins
        .get("run_id")
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            entry
                .event
                .payload
                .get("kind")
                .and_then(|kind| kind.get("run_id"))
                .and_then(serde_json::Value::as_u64)
        })?;
    let terminal = matches!(
        entry.event.kind.as_str(),
        "lightspeed.core.run.completed"
            | "lightspeed.core.run.failed"
            | "lightspeed.core.run.cancelled"
    );
    let is_run = terminal
        || matches!(
            entry.event.kind.as_str(),
            "lightspeed.core.run.accepted"
                | "lightspeed.core.run.started"
                | "lightspeed.core.run.steering_accepted"
                | "lightspeed.core.run.cancellation_requested"
        );
    is_run.then_some(RunBoundary { run_id, terminal })
}

fn commit_uncommitted_events(
    record: &mut SessionRecord,
    events: Vec<DynamicUncommittedSessionEvent>,
) -> Vec<DynamicSessionEntry> {
    let mut committed = Vec::with_capacity(events.len());
    for event in events {
        let next_seq = EventSeq::new(
            record
                .head
                .as_ref()
                .map_or(1, |position| position.seq.as_u64().saturating_add(1)),
        );
        let position = SessionPosition { seq: next_seq };
        let entry = DynamicSessionEntry {
            position: position.clone(),
            observed_at_ms: event.observed_at_ms,
            joins: event.joins,
            event: event.event,
        };
        record.head = Some(position);
        record.updated_at_ms = entry.observed_at_ms;
        committed.push(entry);
    }
    committed
}

fn effective_head_u64(
    inner: &InMemorySessionStoreInner,
    session_id: &SessionId,
) -> Result<u64, SessionStoreError> {
    inner
        .records
        .get(session_id)
        .ok_or_else(|| SessionStoreError::SessionNotFound {
            session_id: session_id.clone(),
        })
        .map(|record| record.head.as_ref().map_or(0, |head| head.seq.as_u64()))
}

fn effective_entries(
    inner: &InMemorySessionStoreInner,
    session_id: &SessionId,
) -> Result<Vec<DynamicSessionEntry>, SessionStoreError> {
    let head = effective_head_u64(inner, session_id)?;
    effective_entries_up_to(inner, session_id, head)
}

fn effective_entries_up_to(
    inner: &InMemorySessionStoreInner,
    session_id: &SessionId,
    max_seq: u64,
) -> Result<Vec<DynamicSessionEntry>, SessionStoreError> {
    let record =
        inner
            .records
            .get(session_id)
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            })?;

    if let (Some(source_session_id), Some(source_seq)) =
        (&record.source_session_id, record.source_seq)
    {
        let branch_seq = source_seq.as_u64();
        if max_seq <= branch_seq {
            return effective_entries_up_to(inner, source_session_id, max_seq);
        }
        let mut entries = effective_entries_up_to(inner, source_session_id, branch_seq)?;
        entries.extend(local_entries_up_to(inner, session_id, branch_seq, max_seq)?);
        return Ok(entries);
    }

    local_entries_up_to(inner, session_id, 0, max_seq)
}

fn local_entries_up_to(
    inner: &InMemorySessionStoreInner,
    session_id: &SessionId,
    after: u64,
    max_seq: u64,
) -> Result<Vec<DynamicSessionEntry>, SessionStoreError> {
    let entries =
        inner
            .entries
            .get(session_id)
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            })?;
    Ok(entries
        .iter()
        .filter(|entry| {
            let seq = entry.position.seq.as_u64();
            seq > after && seq <= max_seq
        })
        .cloned()
        .collect())
}

fn validate_in_memory_fork_point(
    inner: &InMemorySessionStoreInner,
    source_session_id: &SessionId,
    source_seq: EventSeq,
) -> Result<(), SessionStoreError> {
    let entries = effective_entries(inner, source_session_id)?;
    validate_fork_point(
        source_session_id,
        source_seq,
        &entries,
        effective_head_u64(inner, source_session_id)?,
    )
}

fn position_from_nonzero_seq(seq: EventSeq) -> Option<SessionPosition> {
    (seq.as_u64() > 0).then_some(SessionPosition { seq })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{DynamicEvent, DynamicJoins};

    fn test_event(at_ms: u64, kind: &'static str) -> DynamicUncommittedSessionEvent {
        DynamicUncommittedSessionEvent {
            observed_at_ms: at_ms,
            joins: DynamicJoins::default(),
            event: DynamicEvent::new(kind, 1, serde_json::Value::Object(Default::default())),
        }
    }

    fn open_event(at_ms: u64) -> DynamicUncommittedSessionEvent {
        test_event(at_ms, "lightspeed.test.lifecycle.closed")
    }

    fn run_event(at_ms: u64, kind: &'static str, run_id: u64) -> DynamicUncommittedSessionEvent {
        DynamicUncommittedSessionEvent {
            observed_at_ms: at_ms,
            joins: DynamicJoins::from([("run_id".to_owned(), run_id.to_string())]),
            event: DynamicEvent::new(kind, 1, serde_json::Value::Object(Default::default())),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_session_store_assigns_session_local_sequences() {
        let store = InMemorySessionStore::new();
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create session");

        let result = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![open_event(10), open_event(11)],
            })
            .await
            .expect("append");

        assert_eq!(result.entries[0].position.seq, EventSeq::new(1));
        assert_eq!(result.entries[1].position.seq, EventSeq::new(2));
        assert_eq!(
            result.head.as_ref().map(|head| head.seq),
            Some(EventSeq::new(2))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_session_store_rejects_expected_head_conflict() {
        let store = InMemorySessionStore::new();
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let first = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![open_event(10)],
            })
            .await
            .expect("append first");

        let error = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![open_event(11)],
            })
            .await
            .expect_err("stale append fails");

        assert!(matches!(
            error,
            SessionStoreError::ExpectedHeadMismatch {
                expected: None,
                actual,
                ..
            } if actual == first.head
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_session_store_reports_typed_session_errors() {
        let store = InMemorySessionStore::new();
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create session");

        let duplicate = store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                created_at_ms: 2,
            })
            .await
            .expect_err("duplicate session fails");
        assert!(matches!(
            duplicate,
            SessionStoreError::SessionAlreadyExists { .. }
        ));

        let missing = store
            .append(AppendSessionEvents {
                session_id: SessionId::new("missing"),
                expected_head: None,
                events: vec![open_event(10)],
            })
            .await
            .expect_err("missing session fails");
        assert!(matches!(missing, SessionStoreError::SessionNotFound { .. }));

        let conflict = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: Some(SessionPosition {
                    seq: EventSeq::new(1),
                }),
                events: vec![open_event(11)],
            })
            .await
            .expect_err("expected-head conflict fails");
        assert!(matches!(
            conflict,
            SessionStoreError::ExpectedHeadMismatch { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_session_store_clones_with_fresh_log_and_source_lineage() {
        let store = InMemorySessionStore::new();
        let source_id = SessionId::new("source");
        store
            .create_session(CreateSession {
                session_id: source_id.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        store
            .append(AppendSessionEvents {
                session_id: source_id.clone(),
                expected_head: None,
                events: vec![open_event(10), open_event(11)],
            })
            .await
            .expect("append source");

        let child_id = SessionId::new("clone");
        let child = store
            .create_cloned_session(CreateClonedSession {
                source_session_id: source_id.clone(),
                session_id: child_id.clone(),
                created_at_ms: 20,
                opening_events: vec![test_event(21, "lightspeed.test.clone.opened")],
            })
            .await
            .expect("clone session");

        assert_eq!(child.source_session_id, Some(source_id));
        assert_eq!(child.source_seq, None);
        assert_eq!(
            child.head.as_ref().map(|head| head.seq),
            Some(EventSeq::new(1))
        );
        let page = store
            .read_after(ReadSessionEvents {
                session_id: child_id,
                after: None,
                limit: 10,
            })
            .await
            .expect("read clone");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].position.seq, EventSeq::new(1));
        assert_eq!(page.entries[0].event.kind, "lightspeed.test.clone.opened");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_session_links_preserve_direction_and_relationship() {
        let store = InMemorySessionStore::new();
        let left = SessionId::new("left");
        let right = SessionId::new("right");
        for session_id in [&left, &right] {
            store
                .create_session(CreateSession {
                    session_id: session_id.clone(),
                    created_at_ms: 1,
                })
                .await
                .expect("create session");
        }

        let link = store
            .upsert_link(UpsertSessionLink {
                from_session_id: left.clone(),
                to_session_id: right.clone(),
                relationship: "can_see".to_owned(),
                created_at_ms: 10,
                metadata: serde_json::json!({"reason": "test"}),
            })
            .await
            .expect("upsert link");

        assert_eq!(link.from_session_id, left);
        assert_eq!(link.to_session_id, right);
        let outgoing = store
            .list_links(ListSessionLinks {
                session_id: left,
                direction: SessionLinkDirection::Outgoing,
                relationship: Some("can_see".to_owned()),
                limit: 10,
            })
            .await
            .expect("list outgoing");
        assert_eq!(outgoing, vec![link.clone()]);
        let incoming = store
            .list_links(ListSessionLinks {
                session_id: right,
                direction: SessionLinkDirection::Incoming,
                relationship: None,
                limit: 10,
            })
            .await
            .expect("list incoming");
        assert_eq!(incoming, vec![link]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_fork_reads_stitch_multiple_levels_and_clamp_parent_tail() {
        let store = InMemorySessionStore::new();
        let root = SessionId::new("root");
        store
            .create_session(CreateSession {
                session_id: root.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create root");
        store
            .append(AppendSessionEvents {
                session_id: root.clone(),
                expected_head: None,
                events: vec![
                    test_event(10, "root.1"),
                    test_event(11, "root.2"),
                    test_event(12, "root.3"),
                ],
            })
            .await
            .expect("append root");

        let fork = SessionId::new("fork");
        store
            .create_forked_session(CreateForkedSession {
                source_session_id: root.clone(),
                session_id: fork.clone(),
                source_seq: EventSeq::new(2),
                created_at_ms: 20,
            })
            .await
            .expect("fork root");
        let fork_append = store
            .append(AppendSessionEvents {
                session_id: fork.clone(),
                expected_head: Some(SessionPosition {
                    seq: EventSeq::new(2),
                }),
                events: vec![test_event(21, "fork.3"), test_event(22, "fork.4")],
            })
            .await
            .expect("append fork");
        assert_eq!(
            fork_append
                .entries
                .iter()
                .map(|entry| entry.position.seq)
                .collect::<Vec<_>>(),
            vec![EventSeq::new(3), EventSeq::new(4)]
        );

        store
            .append(AppendSessionEvents {
                session_id: root,
                expected_head: Some(SessionPosition {
                    seq: EventSeq::new(3),
                }),
                events: vec![test_event(30, "root.4-hidden")],
            })
            .await
            .expect("append root tail");

        let grandchild = SessionId::new("grandchild");
        store
            .create_forked_session(CreateForkedSession {
                source_session_id: fork.clone(),
                session_id: grandchild.clone(),
                source_seq: EventSeq::new(3),
                created_at_ms: 40,
            })
            .await
            .expect("fork fork");
        store
            .append(AppendSessionEvents {
                session_id: grandchild.clone(),
                expected_head: Some(SessionPosition {
                    seq: EventSeq::new(3),
                }),
                events: vec![test_event(41, "grandchild.4")],
            })
            .await
            .expect("append grandchild");

        let page = store
            .read_after(ReadSessionEvents {
                session_id: grandchild,
                after: Some(EventSeq::new(1)),
                limit: 10,
            })
            .await
            .expect("read grandchild");
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| (entry.position.seq.as_u64(), entry.event.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![(2, "root.2"), (3, "fork.3"), (4, "grandchild.4")]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_safe_fork_seq_excludes_open_run_and_rejects_inside_run() {
        let store = InMemorySessionStore::new();
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![
                    test_event(10, "standalone.1"),
                    run_event(11, "lightspeed.core.run.accepted", 1),
                    run_event(12, "lightspeed.core.run.started", 1),
                ],
            })
            .await
            .expect("append open run");

        assert_eq!(
            store
                .safe_fork_seq(&session_id)
                .await
                .expect("safe fork seq"),
            EventSeq::new(1)
        );
        let error = store
            .create_forked_session(CreateForkedSession {
                source_session_id: session_id.clone(),
                session_id: SessionId::new("bad-fork"),
                source_seq: EventSeq::new(2),
                created_at_ms: 20,
            })
            .await
            .expect_err("fork inside open run fails");
        assert!(matches!(
            error,
            SessionStoreError::InvalidForkPoint {
                source_seq,
                ..
            } if source_seq == EventSeq::new(2)
        ));
    }
}
