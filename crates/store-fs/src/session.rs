use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use engine::{
    session::{DynamicSessionEntry, EventSeq, SessionId, SessionPosition},
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, CreateSession, ListAgentSessions,
        ReadSessionEvents, SessionPage, SessionRecord, SessionStore, SessionStoreError,
    },
};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};

#[derive(Clone)]
pub struct FsSessionStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl FsSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self::new(root);
        fs::create_dir_all(store.sessions_root()).await?;
        Ok(store)
    }

    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(crate::forge_dir(project_root))
    }

    pub async fn open_project(project_root: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(crate::forge_dir(project_root)).await
    }

    pub fn root(&self) -> &Path {
        self.root.as_ref().as_path()
    }

    fn sessions_root(&self) -> PathBuf {
        crate::sessions_dir(self.root())
    }

    fn session_dir(&self, session_id: &SessionId) -> PathBuf {
        self.sessions_root()
            .join(crate::encode_component(session_id.as_str()))
    }

    fn record_path(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join("session.json")
    }

    fn events_path(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join("events.jsonl")
    }

    async fn load_reconciled_record(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let record_path = self.record_path(session_id);
        let Some(record) = read_session_record(&record_path).await? else {
            return Ok(None);
        };
        let entries = self.read_entries_unlocked(session_id).await?;
        Ok(Some(reconcile_record(record, &entries)))
    }

    async fn read_entries_unlocked(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<DynamicSessionEntry>, SessionStoreError> {
        let events_path = self.events_path(session_id);
        let content = fs::read_to_string(&events_path).await.map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                SessionStoreError::Store {
                    message: format!("missing session event log '{}'", events_path.display()),
                }
            } else {
                session_io_error("read session event log", &events_path, error)
            }
        })?;

        let mut entries = Vec::new();
        for (index, line) in content.lines().enumerate() {
            let entry: DynamicSessionEntry =
                serde_json::from_str(line).map_err(|error| SessionStoreError::Store {
                    message: format!(
                        "decode session event log '{}' line {}: {error}",
                        events_path.display(),
                        index + 1
                    ),
                })?;
            let expected_seq = EventSeq::new(entries.len() as u64 + 1);
            if entry.position.seq != expected_seq {
                return Err(SessionStoreError::Store {
                    message: format!(
                        "session event log '{}' line {} has seq {}, expected {}",
                        events_path.display(),
                        index + 1,
                        entry.position.seq,
                        expected_seq
                    ),
                });
            }
            entries.push(entry);
        }
        Ok(entries)
    }
}

#[async_trait]
impl SessionStore for FsSessionStore {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        let _guard = self.lock.lock().await;
        let sessions_root = self.sessions_root();
        fs::create_dir_all(&sessions_root).await.map_err(|error| {
            session_io_error("create sessions directory", &sessions_root, error)
        })?;

        let session_dir = self.session_dir(&request.session_id);
        match fs::create_dir(&session_dir).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(SessionStoreError::SessionAlreadyExists {
                    session_id: request.session_id,
                });
            }
            Err(error) => {
                return Err(session_io_error(
                    "create session directory",
                    &session_dir,
                    error,
                ));
            }
        }

        let record = SessionRecord {
            session_id: request.session_id.clone(),
            agent_handle: request.agent_handle,
            head: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        let events_path = self.events_path(&request.session_id);
        let record_path = self.record_path(&request.session_id);

        if let Err(error) = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&events_path)
            .await
        {
            let _ = fs::remove_dir_all(&session_dir).await;
            return Err(session_io_error(
                "create session event log",
                &events_path,
                error,
            ));
        }

        if let Err(error) = write_session_record(&record_path, &record).await {
            let _ = fs::remove_dir_all(&session_dir).await;
            return Err(error);
        }

        Ok(record)
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let _guard = self.lock.lock().await;
        self.load_reconciled_record(session_id).await
    }

    async fn list_agent_sessions(
        &self,
        request: ListAgentSessions,
    ) -> Result<Vec<SessionRecord>, SessionStoreError> {
        let _guard = self.lock.lock().await;
        let sessions_root = self.sessions_root();
        let mut read_dir = match fs::read_dir(&sessions_root).await {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(session_io_error(
                    "read sessions directory",
                    &sessions_root,
                    error,
                ));
            }
        };

        let mut records = Vec::new();
        while let Some(entry) = read_dir.next_entry().await.map_err(|error| {
            session_io_error("read sessions directory entry", &sessions_root, error)
        })? {
            let file_type = entry.file_type().await.map_err(|error| {
                session_io_error("read sessions directory entry type", &entry.path(), error)
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let record_path = entry.path().join("session.json");
            let Some(record) = read_session_record(&record_path).await? else {
                continue;
            };
            if record.agent_handle == request.agent_handle {
                let entries = self.read_entries_unlocked(&record.session_id).await?;
                records.push(reconcile_record(record, &entries));
            }
        }

        records.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        records.truncate(request.limit);
        Ok(records)
    }

    async fn append(
        &self,
        request: AppendSessionEvents,
    ) -> Result<AppendSessionEventsResult, SessionStoreError> {
        let _guard = self.lock.lock().await;
        let Some(mut record) = self.load_reconciled_record(&request.session_id).await? else {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.session_id,
            });
        };

        let actual_head = record.head.clone();
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

        if !committed.is_empty() {
            let events_path = self.events_path(&request.session_id);
            append_entries(&events_path, &committed).await?;
            record.head = head.clone();
            record.updated_at_ms = committed
                .last()
                .map_or(record.updated_at_ms, |entry| entry.observed_at_ms);
            let record_path = self.record_path(&request.session_id);
            write_session_record(&record_path, &record).await?;
        }

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

        let _guard = self.lock.lock().await;
        if read_session_record(&self.record_path(&request.session_id))
            .await?
            .is_none()
        {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.session_id,
            });
        }
        let mut selected = self
            .read_entries_unlocked(&request.session_id)
            .await?
            .into_iter()
            .filter(|entry| request.after.is_none_or(|after| entry.position.seq > after))
            .take(request.limit.saturating_add(1))
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
        let _guard = self.lock.lock().await;
        Ok(self
            .load_reconciled_record(session_id)
            .await?
            .and_then(|record| record.head))
    }
}

fn reconcile_record(mut record: SessionRecord, entries: &[DynamicSessionEntry]) -> SessionRecord {
    if let Some(last) = entries.last() {
        record.head = Some(last.position.clone());
        record.updated_at_ms = last.observed_at_ms;
    } else {
        record.head = None;
        record.updated_at_ms = record.created_at_ms;
    }
    record
}

async fn append_entries(
    path: &Path,
    entries: &[DynamicSessionEntry],
) -> Result<(), SessionStoreError> {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .await
        .map_err(|error| session_io_error("open session event log", path, error))?;

    for entry in entries {
        let line = serde_json::to_vec(entry).map_err(|error| SessionStoreError::Store {
            message: format!(
                "serialize session event entry for '{}': {error}",
                path.display()
            ),
        })?;
        file.write_all(&line)
            .await
            .map_err(|error| session_io_error("write session event entry", path, error))?;
        file.write_all(b"\n")
            .await
            .map_err(|error| session_io_error("write session event newline", path, error))?;
    }
    file.flush()
        .await
        .map_err(|error| session_io_error("flush session event log", path, error))
}

async fn read_session_record(path: &Path) -> Result<Option<SessionRecord>, SessionStoreError> {
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(session_io_error("read session record", path, error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| SessionStoreError::Store {
            message: format!("decode session record '{}': {error}", path.display()),
        })
}

async fn write_session_record(
    path: &Path,
    record: &SessionRecord,
) -> Result<(), SessionStoreError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| SessionStoreError::Store {
        message: format!("serialize session record for '{}': {error}", path.display()),
    })?;
    crate::atomic_write(path, &bytes)
        .await
        .map_err(|error| session_io_error("write session record", path, error))
}

fn session_io_error(action: &str, path: &Path, error: io::Error) -> SessionStoreError {
    SessionStoreError::Store {
        message: format!("{action} '{}': {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::session::{
        AgentHandle, DynamicEvent, DynamicJoins, DynamicUncommittedSessionEvent,
    };
    use engine::storage::SessionStore;

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
    async fn fs_session_store_persists_session_log() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsSessionStore::open(temp_dir.path())
            .await
            .expect("open store");
        let session_id = SessionId::new("session-a");
        let agent_handle = AgentHandle::new("forge.default");

        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: agent_handle.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let appended = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![open_event(10), open_event(11)],
            })
            .await
            .expect("append events");

        assert_eq!(appended.entries[0].position.seq, EventSeq::new(1));
        assert_eq!(appended.entries[1].position.seq, EventSeq::new(2));

        let reopened = FsSessionStore::open(temp_dir.path())
            .await
            .expect("reopen store");
        let loaded = reopened
            .load_session(&session_id)
            .await
            .expect("load session")
            .expect("session exists");
        assert_eq!(loaded.head, appended.head);
        assert_eq!(loaded.updated_at_ms, 11);

        let page = reopened
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after: None,
                limit: 1,
            })
            .await
            .expect("read page");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.next_after, Some(EventSeq::new(1)));
        assert!(!page.complete);

        assert_eq!(
            reopened
                .list_agent_sessions(ListAgentSessions {
                    agent_handle,
                    limit: 10,
                })
                .await
                .expect("list sessions")
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_session_store_rejects_duplicate_missing_and_stale_writes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsSessionStore::open(temp_dir.path())
            .await
            .expect("open store");
        let session_id = SessionId::new("session-a");
        let agent_handle = AgentHandle::new("forge.default");

        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: agent_handle.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let duplicate = store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle,
                created_at_ms: 2,
            })
            .await
            .expect_err("duplicate fails");
        assert!(matches!(
            duplicate,
            SessionStoreError::SessionAlreadyExists { .. }
        ));

        let first = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![open_event(10)],
            })
            .await
            .expect("append first");
        let stale = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![open_event(11)],
            })
            .await
            .expect_err("stale append fails");
        assert!(matches!(
            stale,
            SessionStoreError::ExpectedHeadMismatch {
                expected: None,
                actual,
                ..
            } if actual == first.head
        ));

        let missing = store
            .append(AppendSessionEvents {
                session_id: SessionId::new("missing"),
                expected_head: None,
                events: vec![open_event(12)],
            })
            .await
            .expect_err("missing append fails");
        assert!(matches!(missing, SessionStoreError::SessionNotFound { .. }));
    }
}
