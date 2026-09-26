use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use engine::{
    session::{EventSeq, SessionId, SessionPosition, StoredSessionEntry},
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, CreateSession, DeleteClosedSessions,
        DeleteClosedSessionsResult, ReadSessionEvents, SessionLifecycleStatus, SessionPage,
        SessionRecord, SessionStore, SessionStoreError, apply_lifecycle_projection,
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
        Self::new(crate::lightspeed_dir(project_root))
    }

    pub async fn open_project(project_root: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(crate::lightspeed_dir(project_root)).await
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
    ) -> Result<Vec<StoredSessionEntry>, SessionStoreError> {
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
            let entry: StoredSessionEntry =
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

    async fn load_all_records_unlocked(
        &self,
    ) -> Result<BTreeMap<SessionId, SessionRecord>, SessionStoreError> {
        let mut records = BTreeMap::new();
        let mut entries = match fs::read_dir(self.sessions_root()).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(records),
            Err(error) => {
                return Err(session_io_error(
                    "read sessions directory",
                    &self.sessions_root(),
                    error,
                ));
            }
        };
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            session_io_error(
                "read sessions directory entry",
                &self.sessions_root(),
                error,
            )
        })? {
            let path = entry.path().join("session.json");
            let Some(record) = read_session_record(&path).await? else {
                continue;
            };
            let session_id = record.session_id.clone();
            let events = self.read_entries_unlocked(&session_id).await?;
            records.insert(session_id, reconcile_record(record, &events));
        }
        Ok(records)
    }
}

#[async_trait]
impl SessionStore for FsSessionStore {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        let _guard = self.lock.lock().await;
        if request.delete_after_close_ms == Some(0) {
            return Err(SessionStoreError::InvalidRetention {
                message: "deleteAfterCloseMs must be positive".to_owned(),
            });
        }
        let retention_root_session_id = if let Some(origin) = request.origin.as_ref() {
            if request.delete_after_close_ms.is_some() {
                return Err(SessionStoreError::SessionRetentionOwnedBy {
                    session_id: request.session_id,
                    retention_root_session_id: origin.root_session_id.clone(),
                });
            }
            let parent = self
                .load_reconciled_record(&origin.parent_session_id)
                .await?
                .ok_or_else(|| SessionStoreError::SessionNotFound {
                    session_id: origin.parent_session_id.clone(),
                })?;
            parent.retention_root_session_id
        } else {
            request.session_id.clone()
        };
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
            display_name: request.display_name,
            metadata: request.metadata,
            lifecycle_status: SessionLifecycleStatus::New,
            closed_at_seq: None,
            closed_at_ms: None,
            activity: Default::default(),
            retention_root_session_id,
            delete_after_close_ms: request.delete_after_close_ms,
            delete_at_ms: None,
            managed: false,
            head: None,
            source_session_id: None,
            source_seq: None,
            origin: request.origin,
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

    async fn set_session_retention(
        &self,
        session_id: &SessionId,
        delete_after_close_ms: Option<u64>,
    ) -> Result<SessionRecord, SessionStoreError> {
        if delete_after_close_ms == Some(0) {
            return Err(SessionStoreError::InvalidRetention {
                message: "deleteAfterCloseMs must be positive".to_owned(),
            });
        }
        let _guard = self.lock.lock().await;
        let Some(mut record) = self.load_reconciled_record(session_id).await? else {
            return Err(SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            });
        };
        if record.retention_root_session_id != *session_id {
            return Err(SessionStoreError::SessionRetentionOwnedBy {
                session_id: session_id.clone(),
                retention_root_session_id: record.retention_root_session_id,
            });
        }
        record.delete_after_close_ms = delete_after_close_ms;
        record.delete_at_ms = record
            .closed_at_ms
            .zip(delete_after_close_ms)
            .map(|(closed_at_ms, duration_ms)| closed_at_ms.saturating_add(duration_ms));
        write_session_record(&self.record_path(session_id), &record).await?;
        Ok(record)
    }

    async fn list_retention_roots_due_for_deletion(
        &self,
        now_ms: u64,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, SessionStoreError> {
        if limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit });
        }
        let _guard = self.lock.lock().await;
        let mut due = self
            .load_all_records_unlocked()
            .await?
            .into_values()
            .filter(|record| {
                record.retention_root_session_id == record.session_id
                    && record.lifecycle_status == SessionLifecycleStatus::Closed
                    && record
                        .delete_at_ms
                        .is_some_and(|deadline| deadline <= now_ms)
            })
            .collect::<Vec<_>>();
        due.sort_by(|left, right| {
            left.delete_at_ms
                .cmp(&right.delete_at_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        due.truncate(limit);
        Ok(due)
    }

    async fn delete_closed_sessions(
        &self,
        request: DeleteClosedSessions,
    ) -> Result<DeleteClosedSessionsResult, SessionStoreError> {
        let _guard = self.lock.lock().await;
        let records = self.load_all_records_unlocked().await?;
        let target = records.get(&request.session_id).cloned().ok_or_else(|| {
            SessionStoreError::SessionNotFound {
                session_id: request.session_id.clone(),
            }
        })?;
        if let Some(now_ms) = request.due_at_or_before_ms
            && (target.retention_root_session_id != target.session_id
                || !target
                    .delete_at_ms
                    .is_some_and(|deadline| deadline <= now_ms))
        {
            return Err(SessionStoreError::SessionRetentionNotDue {
                session_id: target.session_id,
            });
        }
        let mut descendants = retention_descendants(&records, &request.session_id);
        if !request.cascade && !descendants.is_empty() {
            return Err(SessionStoreError::SessionHasChildren {
                session_id: request.session_id,
            });
        }
        if request.cascade {
            descendants.push(target.session_id.clone());
        } else {
            descendants = vec![target.session_id.clone()];
        }
        for session_id in &descendants {
            let record = records
                .get(session_id)
                .expect("retention descendant exists");
            if record.lifecycle_status != SessionLifecycleStatus::Closed {
                return Err(if *session_id == request.session_id {
                    SessionStoreError::SessionNotClosed {
                        session_id: session_id.clone(),
                        lifecycle_status: record.lifecycle_status,
                    }
                } else {
                    SessionStoreError::SessionTreeNotClosed {
                        session_id: session_id.clone(),
                        lifecycle_status: record.lifecycle_status,
                    }
                });
            }
        }
        for session_id in &descendants {
            fs::remove_dir_all(self.session_dir(session_id))
                .await
                .map_err(|error| {
                    session_io_error(
                        "delete session directory",
                        &self.session_dir(session_id),
                        error,
                    )
                })?;
        }
        for mut record in records.into_values() {
            if descendants.contains(&record.session_id) {
                continue;
            }
            if record
                .source_session_id
                .as_ref()
                .is_some_and(|source| descendants.contains(source))
            {
                record.source_session_id = None;
                write_session_record(&self.record_path(&record.session_id), &record).await?;
            }
        }
        Ok(DeleteClosedSessionsResult {
            target,
            deleted_session_ids: descendants,
        })
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
            let entry = StoredSessionEntry {
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
            for entry in &committed {
                apply_lifecycle_projection(&mut record, entry);
            }
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

fn reconcile_record(mut record: SessionRecord, entries: &[StoredSessionEntry]) -> SessionRecord {
    record.lifecycle_status = SessionLifecycleStatus::New;
    record.closed_at_seq = None;
    record.closed_at_ms = None;
    record.delete_at_ms = None;
    record.managed = false;
    for entry in entries {
        apply_lifecycle_projection(&mut record, entry);
    }
    if let Some(last) = entries.last() {
        record.head = Some(last.position.clone());
        record.updated_at_ms = last.observed_at_ms;
    } else {
        record.head = None;
        record.updated_at_ms = record.created_at_ms;
    }
    record
}

fn retention_descendants(
    records: &BTreeMap<SessionId, SessionRecord>,
    session_id: &SessionId,
) -> Vec<SessionId> {
    let mut descendants = Vec::new();
    let mut pending = vec![session_id.clone()];
    while let Some(parent) = pending.pop() {
        let children = records
            .values()
            .filter(|candidate| {
                candidate
                    .origin
                    .as_ref()
                    .is_some_and(|origin| origin.parent_session_id == parent)
                    || (candidate.source_seq.is_some()
                        && candidate.source_session_id.as_ref() == Some(&parent))
            })
            .map(|candidate| candidate.session_id.clone())
            .collect::<Vec<_>>();
        for child in children {
            if !descendants.contains(&child) {
                pending.push(child.clone());
                descendants.push(child);
            }
        }
    }
    descendants.reverse();
    descendants
}

async fn append_entries(
    path: &Path,
    entries: &[StoredSessionEntry],
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
    use engine::session::{StoredEvent, StoredJoins, UncommittedStoredEvent};
    use engine::storage::SessionStore;

    fn open_event(at_ms: u64) -> UncommittedStoredEvent {
        UncommittedStoredEvent {
            observed_at_ms: at_ms,
            joins: StoredJoins::default(),
            event: StoredEvent::new(
                "lightspeed.test.lifecycle.closed",
                1,
                serde_json::Value::Object(Default::default()),
            ),
        }
    }

    fn lifecycle_event(at_ms: u64, kind: &'static str) -> UncommittedStoredEvent {
        UncommittedStoredEvent {
            observed_at_ms: at_ms,
            joins: StoredJoins::default(),
            event: StoredEvent::new(kind, 1, serde_json::Value::Object(Default::default())),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_session_store_persists_retention_deadlines() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsSessionStore::open(temp_dir.path())
            .await
            .expect("open store");
        let session_id = SessionId::new("retained-session");
        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: Some(100),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![
                    lifecycle_event(10, engine::CORE_AGENT_LIFECYCLE_OPENED_EVENT_KIND),
                    lifecycle_event(20, engine::CORE_AGENT_LIFECYCLE_CLOSED_EVENT_KIND),
                ],
            })
            .await
            .expect("close session");

        let reopened = FsSessionStore::open(temp_dir.path())
            .await
            .expect("reopen store");
        let record = reopened
            .load_session(&session_id)
            .await
            .expect("load session")
            .expect("session exists");
        assert_eq!(record.closed_at_ms, Some(20));
        assert_eq!(record.delete_at_ms, Some(120));
        assert_eq!(
            reopened
                .list_retention_roots_due_for_deletion(120, 1)
                .await
                .expect("list due roots")
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_session_store_persists_session_log() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsSessionStore::open(temp_dir.path())
            .await
            .expect("open store");
        let session_id = SessionId::new("session-a");

        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_session_store_rejects_duplicate_missing_and_stale_writes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsSessionStore::open(temp_dir.path())
            .await
            .expect("open store");
        let session_id = SessionId::new("session-a");

        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let duplicate = store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
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
