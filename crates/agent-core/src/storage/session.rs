//! Session event-log storage contract.

use crate::session::{
    AgentHandle, DynamicSessionEntry, DynamicUncommittedSessionEvent, EventSeq, SessionId,
    SessionPosition,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub agent_handle: AgentHandle,
    pub head: Option<SessionPosition>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSession {
    pub session_id: SessionId,
    pub agent_handle: AgentHandle,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAgentSessions {
    pub agent_handle: AgentHandle,
    pub limit: usize,
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

    async fn list_agent_sessions(
        &self,
        request: ListAgentSessions,
    ) -> Result<Vec<SessionRecord>, SessionStoreError>;

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
            agent_handle: request.agent_handle,
            head: None,
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

    async fn list_agent_sessions(
        &self,
        request: ListAgentSessions,
    ) -> Result<Vec<SessionRecord>, SessionStoreError> {
        let inner = self.inner.read().map_err(|_| SessionStoreError::Store {
            message: "session store read lock poisoned".into(),
        })?;
        Ok(inner
            .records
            .values()
            .filter(|record| record.agent_handle == request.agent_handle)
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

        let mut head = actual_head;
        let mut committed = Vec::with_capacity(request.events.len());
        for event in request.events {
            let next_seq = EventSeq::new(
                head.as_ref()
                    .map_or(1, |position| position.seq.as_u64().saturating_add(1)),
            );
            let position = SessionPosition { seq: next_seq };
            let entry = DynamicSessionEntry {
                position: position.clone(),
                observed_at_ms: event.observed_at_ms,
                joins: event.joins,
                event: event.event,
            };
            head = Some(position);
            committed.push(entry);
        }

        let entries = inner
            .entries
            .get_mut(&request.session_id)
            .expect("session entries exist for record");
        entries.extend(committed.clone());
        let record = inner
            .records
            .get_mut(&request.session_id)
            .expect("validated session record");
        if let Some(last) = committed.last() {
            record.updated_at_ms = last.observed_at_ms;
        }
        record.head = head.clone();

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
        let entries = inner.entries.get(&request.session_id).ok_or_else(|| {
            SessionStoreError::SessionNotFound {
                session_id: request.session_id.clone(),
            }
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{DynamicEvent, DynamicJoins};

    fn open_event(at_ms: u64) -> DynamicUncommittedSessionEvent {
        DynamicUncommittedSessionEvent {
            observed_at_ms: at_ms,
            joins: DynamicJoins::default(),
            event: DynamicEvent::new(
                "forge.test.lifecycle.closed",
                1,
                serde_json::Value::Object(Default::default()),
            ),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_session_store_assigns_session_local_sequences() {
        let store = InMemorySessionStore::new();
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
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
                agent_handle: AgentHandle::new("forge.default"),
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
                agent_handle: AgentHandle::new("forge.default"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");

        let duplicate = store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.default"),
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
}
