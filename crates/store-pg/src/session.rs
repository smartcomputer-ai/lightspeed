use async_trait::async_trait;
use engine::{
    session::{AgentHandle, DynamicSessionEntry, EventSeq, SessionId, SessionPosition},
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, CreateSession, ListAgentSessions,
        ReadSessionEvents, SessionPage, SessionRecord, SessionStore, SessionStoreError,
    },
};
use sqlx::Row;

use crate::{
    PgStore,
    shared::{
        event_seq_to_i64, i64_to_u64, session_position_from_i64, session_sql_error,
        session_store_error, u64_to_i64, usize_to_session_i64,
    },
};

impl PgStore {
    async fn append_inner(
        &self,
        request: AppendSessionEvents,
    ) -> Result<AppendSessionEventsResult, SessionStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| session_sql_error("begin append transaction", error))?;
        let row = sqlx::query(
            r#"
            SELECT session_id, agent_handle, head_seq, created_at_ms, updated_at_ms
            FROM sessions
            WHERE universe_id = $1 AND session_id = $2
            FOR UPDATE
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.session_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| session_sql_error("load session for append", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.session_id,
            });
        };
        let mut record = session_record_from_row(&row)?;
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
            let entry_json =
                serde_json::to_value(&entry).map_err(|error| SessionStoreError::Store {
                    message: format!("serialize session entry: {error}"),
                })?;
            sqlx::query(
                r#"
                INSERT INTO session_events (universe_id, session_id, entry_json)
                VALUES ($1, $2, $3)
                "#,
            )
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(entry_json)
            .execute(&mut *tx)
            .await
            .map_err(|error| session_sql_error("insert session event", error))?;

            head = Some(position);
            committed.push(entry);
        }

        if let Some(last) = committed.last() {
            record.updated_at_ms = last.observed_at_ms;
            sqlx::query(
                r#"
                UPDATE sessions
                SET head_seq = $3, updated_at_ms = $4, modified_at = now()
                WHERE universe_id = $1 AND session_id = $2
                "#,
            )
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(event_seq_to_i64(last.position.seq)?)
            .bind(u64_to_i64(last.observed_at_ms, "updated_at_ms")?)
            .execute(&mut *tx)
            .await
            .map_err(|error| session_sql_error("update session head", error))?;
        }

        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit append transaction", error))?;
        Ok(AppendSessionEventsResult {
            entries: committed,
            head,
        })
    }
}

#[async_trait]
impl SessionStore for PgStore {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        self.ensure_universe()
            .await
            .map_err(|error| session_store_error("ensure universe", error))?;
        let created_at_ms = u64_to_i64(request.created_at_ms, "created_at_ms")?;
        let row = sqlx::query(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                agent_handle,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (universe_id, session_id) DO NOTHING
            RETURNING session_id, agent_handle, head_seq, created_at_ms, updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.session_id.as_str())
        .bind(request.agent_handle.as_str())
        .bind(created_at_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| session_sql_error("create session", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        };
        session_record_from_row(&row)
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        let row = sqlx::query(
            r#"
            SELECT session_id, agent_handle, head_seq, created_at_ms, updated_at_ms
            FROM sessions
            WHERE universe_id = $1 AND session_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(session_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| session_sql_error("load session", error))?;

        row.as_ref().map(session_record_from_row).transpose()
    }

    async fn list_agent_sessions(
        &self,
        request: ListAgentSessions,
    ) -> Result<Vec<SessionRecord>, SessionStoreError> {
        let limit = usize_to_session_i64(request.limit, "session list limit")?;
        let rows = sqlx::query(
            r#"
            SELECT session_id, agent_handle, head_seq, created_at_ms, updated_at_ms
            FROM sessions
            WHERE universe_id = $1 AND agent_handle = $2
            ORDER BY session_id
            LIMIT $3
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.agent_handle.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| session_sql_error("list agent sessions", error))?;

        rows.iter().map(session_record_from_row).collect()
    }

    async fn append(
        &self,
        request: AppendSessionEvents,
    ) -> Result<AppendSessionEventsResult, SessionStoreError> {
        self.append_inner(request).await
    }

    async fn read_after(
        &self,
        request: ReadSessionEvents,
    ) -> Result<SessionPage, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        let exists = sqlx::query(
            r#"
            SELECT 1
            FROM sessions
            WHERE universe_id = $1 AND session_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.session_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| session_sql_error("check session existence", error))?
        .is_some();
        if !exists {
            return Err(SessionStoreError::SessionNotFound {
                session_id: request.session_id,
            });
        }

        let after = request.after.map_or(0, |seq| seq.as_u64());
        let rows = sqlx::query(
            r#"
            SELECT entry_json
            FROM session_events
            WHERE universe_id = $1 AND session_id = $2 AND seq > $3
            ORDER BY seq
            LIMIT $4
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.session_id.as_str())
        .bind(u64_to_i64(after, "read_after seq")?)
        .bind(usize_to_session_i64(
            request.limit.saturating_add(1),
            "read_after limit",
        )?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| session_sql_error("read session events", error))?;

        let mut selected = Vec::with_capacity(rows.len().min(request.limit));
        for row in rows {
            let entry_json: serde_json::Value = row
                .try_get("entry_json")
                .map_err(|error| session_sql_error("decode session event json", error))?;
            let entry =
                serde_json::from_value::<DynamicSessionEntry>(entry_json).map_err(|error| {
                    SessionStoreError::Store {
                        message: format!("decode session event entry: {error}"),
                    }
                })?;
            selected.push(entry);
        }

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
        self.load_session(session_id)
            .await
            .map(|record| record.and_then(|record| record.head))
    }
}

fn session_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<SessionRecord, SessionStoreError> {
    let session_id = row
        .try_get::<String, _>("session_id")
        .map_err(|error| session_sql_error("decode session id", error))
        .and_then(|value| {
            SessionId::parse(value).map_err(|error| SessionStoreError::Store {
                message: format!("decode session id: {error}"),
            })
        })?;
    let agent_handle = row
        .try_get::<String, _>("agent_handle")
        .map_err(|error| session_sql_error("decode agent handle", error))
        .and_then(|value| {
            AgentHandle::parse(value).map_err(|error| SessionStoreError::Store {
                message: format!("decode agent handle: {error}"),
            })
        })?;
    let head_seq = row
        .try_get::<Option<i64>, _>("head_seq")
        .map_err(|error| session_sql_error("decode session head", error))?;
    let created_at_ms = row
        .try_get::<i64, _>("created_at_ms")
        .map_err(|error| session_sql_error("decode created_at_ms", error))
        .and_then(|value| {
            i64_to_u64(value, "created_at_ms")
                .map_err(|message| SessionStoreError::Store { message })
        })?;
    let updated_at_ms = row
        .try_get::<i64, _>("updated_at_ms")
        .map_err(|error| session_sql_error("decode updated_at_ms", error))
        .and_then(|value| {
            i64_to_u64(value, "updated_at_ms")
                .map_err(|message| SessionStoreError::Store { message })
        })?;
    let head = session_position_from_i64(head_seq)?;

    Ok(SessionRecord {
        session_id,
        agent_handle,
        head,
        created_at_ms,
        updated_at_ms,
    })
}
