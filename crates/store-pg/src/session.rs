use async_trait::async_trait;
use engine::{
    session::{EventSeq, SessionId, SessionPosition, StoredSessionEntry, UncommittedStoredEvent},
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, CreateClonedSession, CreateForkedSession,
        CreateSession, ListSessionLinks, ListSessions, ReadSessionEvents, SessionLinkDirection,
        SessionLinkRecord, SessionListCursor, SessionListPage, SessionPage, SessionRecord,
        SessionStore, SessionStoreError, UpsertSessionLink, largest_safe_fork_seq,
        validate_fork_point, validate_relationship,
    },
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    PgStore,
    shared::{
        event_seq_to_i64, i64_to_u64, session_position_from_i64, session_sql_error,
        session_store_error, u64_to_i64, usize_to_session_i64,
    },
};

const SESSION_COLUMNS: &str = r#"
    session_id,
    display_name,
    head_seq,
    source_session_id,
    source_seq,
    created_at_ms,
    updated_at_ms
"#;

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
        let query = format!(
            r#"
            SELECT {SESSION_COLUMNS}
            FROM sessions
            WHERE universe_id = $1 AND session_id = $2
            FOR UPDATE
            "#,
        );
        let row = sqlx::query(&query)
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
            let entry = StoredSessionEntry {
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
                SET head_seq = $3, updated_at_ms = $4
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

    pub async fn copy_session_resources(
        &self,
        source_session_id: &SessionId,
        child_session_id: &SessionId,
    ) -> Result<(), SessionStoreError> {
        self.ensure_universe()
            .await
            .map_err(|error| session_store_error("ensure universe", error))?;
        let mut tx = self.pool.begin().await.map_err(|error| {
            session_sql_error("begin copy session resources transaction", error)
        })?;
        lock_session(
            &mut tx,
            self.config.universe_id,
            source_session_id,
            "copy resources source",
        )
        .await?;
        lock_session(
            &mut tx,
            self.config.universe_id,
            child_session_id,
            "copy resources child",
        )
        .await?;
        copy_session_resources_in_tx(
            &mut tx,
            self.config.universe_id,
            source_session_id,
            child_session_id,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit copy session resources", error))?;
        Ok(())
    }

    async fn read_all_effective_events(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<StoredSessionEntry>, SessionStoreError> {
        let head = self
            .load_session(session_id)
            .await?
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            })?
            .head
            .map_or(0, |head| head.seq.as_u64());
        let mut entries = Vec::new();
        let mut after = 0;
        while after < head {
            let page = self.read_effective_window(session_id, after, 512).await?;
            if page.is_empty() {
                return Err(SessionStoreError::Store {
                    message: format!(
                        "session {session_id} effective log has a gap after seq {after}"
                    ),
                });
            }
            after = page
                .last()
                .expect("page checked non-empty")
                .position
                .seq
                .as_u64();
            entries.extend(page);
        }
        Ok(entries)
    }

    async fn read_effective_window(
        &self,
        session_id: &SessionId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredSessionEntry>, SessionStoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let record = self.load_session(session_id).await?.ok_or_else(|| {
            SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            }
        })?;
        let head = record.head.as_ref().map_or(0, |head| head.seq.as_u64());
        let segments = self.resolve_segments(&record.session_id, head).await?;
        let mut selected = Vec::with_capacity(limit);
        for segment in segments {
            if selected.len() >= limit {
                break;
            }
            let lower = segment.after.max(after);
            if segment.through <= lower {
                continue;
            }
            let remaining = limit.saturating_sub(selected.len());
            let mut entries = self
                .read_local_segment(&segment.session_id, lower, segment.through, remaining)
                .await?;
            selected.append(&mut entries);
        }
        Ok(selected)
    }

    async fn resolve_segments(
        &self,
        session_id: &SessionId,
        max_seq: u64,
    ) -> Result<Vec<SessionSegment>, SessionStoreError> {
        enum Task {
            Resolve {
                session_id: SessionId,
                max_seq: u64,
            },
            Local {
                session_id: SessionId,
                after: u64,
                through: u64,
            },
        }

        let mut tasks = vec![Task::Resolve {
            session_id: session_id.clone(),
            max_seq,
        }];
        let mut segments = Vec::new();
        let mut depth = 0usize;

        while let Some(task) = tasks.pop() {
            match task {
                Task::Local {
                    session_id,
                    after,
                    through,
                } => {
                    if through > after {
                        segments.push(SessionSegment {
                            session_id,
                            after,
                            through,
                        });
                    }
                }
                Task::Resolve {
                    session_id,
                    max_seq,
                } => {
                    depth = depth.saturating_add(1);
                    if depth > 256 {
                        return Err(SessionStoreError::Store {
                            message: format!(
                                "session lineage chain is too deep while resolving {session_id}"
                            ),
                        });
                    }
                    let record = self.load_session_required(&session_id).await?;
                    if let (Some(source_session_id), Some(source_seq)) =
                        (record.source_session_id.clone(), record.source_seq)
                    {
                        let branch_seq = source_seq.as_u64();
                        if max_seq <= branch_seq {
                            tasks.push(Task::Resolve {
                                session_id: source_session_id,
                                max_seq,
                            });
                        } else {
                            tasks.push(Task::Local {
                                session_id,
                                after: branch_seq,
                                through: max_seq,
                            });
                            tasks.push(Task::Resolve {
                                session_id: source_session_id,
                                max_seq: branch_seq,
                            });
                        }
                    } else {
                        tasks.push(Task::Local {
                            session_id,
                            after: 0,
                            through: max_seq,
                        });
                    }
                }
            }
        }

        Ok(segments)
    }

    async fn read_local_segment(
        &self,
        session_id: &SessionId,
        after: u64,
        through: u64,
        limit: usize,
    ) -> Result<Vec<StoredSessionEntry>, SessionStoreError> {
        let rows = sqlx::query(
            r#"
            SELECT entry_json
            FROM session_events
            WHERE universe_id = $1
              AND session_id = $2
              AND seq > $3
              AND seq <= $4
            ORDER BY seq
            LIMIT $5
            "#,
        )
        .bind(self.config.universe_id)
        .bind(session_id.as_str())
        .bind(u64_to_i64(after, "read_after seq")?)
        .bind(u64_to_i64(through, "read through seq")?)
        .bind(usize_to_session_i64(limit, "read_after limit")?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| session_sql_error("read session event segment", error))?;

        rows.iter().map(session_entry_from_row).collect()
    }

    async fn load_session_required(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionRecord, SessionStoreError> {
        self.load_session(session_id)
            .await?
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            })
    }
}

#[derive(Clone, Debug)]
struct SessionSegment {
    session_id: SessionId,
    after: u64,
    through: u64,
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
        let query = format!(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                display_name,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (universe_id, session_id) DO NOTHING
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(request.display_name.as_deref())
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
        let query = format!(
            r#"
            SELECT {SESSION_COLUMNS}
            FROM sessions
            WHERE universe_id = $1 AND session_id = $2
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| session_sql_error("load session", error))?;

        row.as_ref().map(session_record_from_row).transpose()
    }

    async fn list_sessions(
        &self,
        request: ListSessions,
    ) -> Result<SessionListPage, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        let fetch_limit = usize_to_session_i64(request.limit.saturating_add(1), "limit")?;
        let rows = match &request.cursor {
            Some(cursor) => {
                let query = format!(
                    r#"
                    SELECT {SESSION_COLUMNS}
                    FROM sessions
                    WHERE universe_id = $1
                      AND (updated_at_ms, session_id) < ($2, $3)
                    ORDER BY updated_at_ms DESC, session_id DESC
                    LIMIT $4
                    "#,
                );
                sqlx::query(&query)
                    .bind(self.config.universe_id)
                    .bind(u64_to_i64(cursor.updated_at_ms, "cursor updated_at_ms")?)
                    .bind(cursor.session_id.as_str())
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                let query = format!(
                    r#"
                    SELECT {SESSION_COLUMNS}
                    FROM sessions
                    WHERE universe_id = $1
                    ORDER BY updated_at_ms DESC, session_id DESC
                    LIMIT $2
                    "#,
                );
                sqlx::query(&query)
                    .bind(self.config.universe_id)
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(|error| session_sql_error("list sessions", error))?;

        let mut sessions = rows
            .iter()
            .map(session_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = (sessions.len() > request.limit).then(|| {
            sessions.truncate(request.limit);
            let last = sessions.last().expect("non-empty page");
            SessionListCursor {
                updated_at_ms: last.updated_at_ms,
                session_id: last.session_id.clone(),
            }
        });
        Ok(SessionListPage {
            sessions,
            next_cursor,
        })
    }

    async fn set_session_display_name(
        &self,
        session_id: &SessionId,
        display_name: Option<String>,
    ) -> Result<SessionRecord, SessionStoreError> {
        let query = format!(
            r#"
            UPDATE sessions
            SET display_name = $3
            WHERE universe_id = $1 AND session_id = $2
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(display_name.as_deref())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| session_sql_error("set session display name", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            });
        };
        session_record_from_row(&row)
    }

    async fn create_cloned_session(
        &self,
        request: CreateClonedSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        self.ensure_universe()
            .await
            .map_err(|error| session_store_error("ensure universe", error))?;
        let created_at_ms = u64_to_i64(request.created_at_ms, "created_at_ms")?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| session_sql_error("begin clone transaction", error))?;

        lock_session(
            &mut tx,
            self.config.universe_id,
            &request.source_session_id,
            "clone source",
        )
        .await?;
        let query = format!(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                source_session_id,
                source_seq,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, NULL, $4, $4)
            ON CONFLICT (universe_id, session_id) DO NOTHING
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(request.source_session_id.as_str())
            .bind(created_at_ms)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| session_sql_error("create cloned session", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        };
        let record = session_record_from_row(&row)?;
        copy_session_resources_in_tx(
            &mut tx,
            self.config.universe_id,
            &request.source_session_id,
            &record.session_id,
        )
        .await?;
        let (record, _) = append_events_in_tx(
            &mut tx,
            self.config.universe_id,
            record,
            request.opening_events,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit clone transaction", error))?;
        Ok(record)
    }

    async fn create_forked_session(
        &self,
        request: CreateForkedSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        self.ensure_universe()
            .await
            .map_err(|error| session_store_error("ensure universe", error))?;
        let source_entries = self
            .read_all_effective_events(&request.source_session_id)
            .await?;
        let source_head = self
            .head(&request.source_session_id)
            .await?
            .map_or(0, |head| head.seq.as_u64());
        validate_fork_point(
            &request.source_session_id,
            request.source_seq,
            &source_entries,
            source_head,
        )?;

        let created_at_ms = u64_to_i64(request.created_at_ms, "created_at_ms")?;
        let source_seq_u64 = request.source_seq.as_u64();
        let head_seq = if source_seq_u64 == 0 {
            None
        } else {
            Some(u64_to_i64(source_seq_u64, "fork head_seq")?)
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| session_sql_error("begin fork transaction", error))?;
        lock_session(
            &mut tx,
            self.config.universe_id,
            &request.source_session_id,
            "fork source",
        )
        .await?;
        let query = format!(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                head_seq,
                source_session_id,
                source_seq,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $6)
            ON CONFLICT (universe_id, session_id) DO NOTHING
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(head_seq)
            .bind(request.source_session_id.as_str())
            .bind(u64_to_i64(source_seq_u64, "source_seq")?)
            .bind(created_at_ms)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| session_sql_error("create forked session", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        };
        let record = session_record_from_row(&row)?;
        copy_session_resources_in_tx(
            &mut tx,
            self.config.universe_id,
            &request.source_session_id,
            &record.session_id,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit fork transaction", error))?;
        Ok(record)
    }

    async fn safe_fork_seq(&self, session_id: &SessionId) -> Result<EventSeq, SessionStoreError> {
        let entries = self.read_all_effective_events(session_id).await?;
        let head = self
            .head(session_id)
            .await?
            .map_or(0, |head| head.seq.as_u64());
        Ok(largest_safe_fork_seq(&entries, head))
    }

    async fn upsert_link(
        &self,
        request: UpsertSessionLink,
    ) -> Result<SessionLinkRecord, SessionStoreError> {
        validate_relationship(&request.relationship)?;
        self.ensure_universe()
            .await
            .map_err(|error| session_store_error("ensure universe", error))?;
        self.load_session_required(&request.from_session_id).await?;
        self.load_session_required(&request.to_session_id).await?;
        let metadata = validate_link_metadata(request.metadata)?;
        let row = sqlx::query(
            r#"
            INSERT INTO session_links (
                universe_id,
                from_session_id,
                to_session_id,
                relationship,
                created_at_ms,
                metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (universe_id, from_session_id, to_session_id, relationship)
            DO UPDATE SET
                created_at_ms = EXCLUDED.created_at_ms,
                metadata = EXCLUDED.metadata
            RETURNING
                from_session_id,
                to_session_id,
                relationship,
                created_at_ms,
                metadata
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.from_session_id.as_str())
        .bind(request.to_session_id.as_str())
        .bind(&request.relationship)
        .bind(u64_to_i64(request.created_at_ms, "link created_at_ms")?)
        .bind(metadata)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| session_sql_error("upsert session link", error))?;
        session_link_from_row(&row)
    }

    async fn list_links(
        &self,
        request: ListSessionLinks,
    ) -> Result<Vec<SessionLinkRecord>, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        self.load_session_required(&request.session_id).await?;
        let limit = usize_to_session_i64(request.limit, "session link list limit")?;
        let rows = match (request.direction, request.relationship.as_ref()) {
            (SessionLinkDirection::Outgoing, Some(relationship)) => {
                sqlx::query(
                    r#"
                    SELECT from_session_id, to_session_id, relationship, created_at_ms, metadata
                    FROM session_links
                    WHERE universe_id = $1 AND from_session_id = $2 AND relationship = $3
                    ORDER BY to_session_id, relationship
                    LIMIT $4
                    "#,
                )
                .bind(self.config.universe_id)
                .bind(request.session_id.as_str())
                .bind(relationship)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            (SessionLinkDirection::Outgoing, None) => {
                sqlx::query(
                    r#"
                    SELECT from_session_id, to_session_id, relationship, created_at_ms, metadata
                    FROM session_links
                    WHERE universe_id = $1 AND from_session_id = $2
                    ORDER BY to_session_id, relationship
                    LIMIT $3
                    "#,
                )
                .bind(self.config.universe_id)
                .bind(request.session_id.as_str())
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            (SessionLinkDirection::Incoming, Some(relationship)) => {
                sqlx::query(
                    r#"
                    SELECT from_session_id, to_session_id, relationship, created_at_ms, metadata
                    FROM session_links
                    WHERE universe_id = $1 AND to_session_id = $2 AND relationship = $3
                    ORDER BY from_session_id, relationship
                    LIMIT $4
                    "#,
                )
                .bind(self.config.universe_id)
                .bind(request.session_id.as_str())
                .bind(relationship)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            (SessionLinkDirection::Incoming, None) => {
                sqlx::query(
                    r#"
                    SELECT from_session_id, to_session_id, relationship, created_at_ms, metadata
                    FROM session_links
                    WHERE universe_id = $1 AND to_session_id = $2
                    ORDER BY from_session_id, relationship
                    LIMIT $3
                    "#,
                )
                .bind(self.config.universe_id)
                .bind(request.session_id.as_str())
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|error| session_sql_error("list session links", error))?;
        rows.iter().map(session_link_from_row).collect()
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

        let after = request.after.map_or(0, |seq| seq.as_u64());
        let mut selected = self
            .read_effective_window(&request.session_id, after, request.limit.saturating_add(1))
            .await?;

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
    let display_name = row
        .try_get::<Option<String>, _>("display_name")
        .map_err(|error| session_sql_error("decode session display name", error))?;
    let head_seq = row
        .try_get::<Option<i64>, _>("head_seq")
        .map_err(|error| session_sql_error("decode session head", error))?;
    let source_session_id = row
        .try_get::<Option<String>, _>("source_session_id")
        .map_err(|error| session_sql_error("decode source session id", error))?
        .map(SessionId::parse)
        .transpose()
        .map_err(|error| SessionStoreError::Store {
            message: format!("decode source session id: {error}"),
        })?;
    let source_seq = row
        .try_get::<Option<i64>, _>("source_seq")
        .map_err(|error| session_sql_error("decode source seq", error))
        .and_then(optional_event_seq_from_i64)?;
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
        display_name,
        head,
        source_session_id,
        source_seq,
        created_at_ms,
        updated_at_ms,
    })
}

fn optional_event_seq_from_i64(seq: Option<i64>) -> Result<Option<EventSeq>, SessionStoreError> {
    seq.map(|seq| {
        i64_to_u64(seq, "source_seq")
            .map(EventSeq::new)
            .map_err(|message| SessionStoreError::Store { message })
    })
    .transpose()
}

fn session_entry_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<StoredSessionEntry, SessionStoreError> {
    let entry_json: serde_json::Value = row
        .try_get("entry_json")
        .map_err(|error| session_sql_error("decode session event json", error))?;
    serde_json::from_value::<StoredSessionEntry>(entry_json).map_err(|error| {
        SessionStoreError::Store {
            message: format!("decode session event entry: {error}"),
        }
    })
}

fn validate_link_metadata(
    metadata: serde_json::Value,
) -> Result<serde_json::Value, SessionStoreError> {
    if metadata.is_object() {
        Ok(metadata)
    } else {
        Err(SessionStoreError::Store {
            message: "session link metadata must be a JSON object".to_owned(),
        })
    }
}

fn session_link_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<SessionLinkRecord, SessionStoreError> {
    let from_session_id = row
        .try_get::<String, _>("from_session_id")
        .map_err(|error| session_sql_error("decode link from session id", error))
        .and_then(|value| {
            SessionId::parse(value).map_err(|error| SessionStoreError::Store {
                message: format!("decode link from session id: {error}"),
            })
        })?;
    let to_session_id = row
        .try_get::<String, _>("to_session_id")
        .map_err(|error| session_sql_error("decode link to session id", error))
        .and_then(|value| {
            SessionId::parse(value).map_err(|error| SessionStoreError::Store {
                message: format!("decode link to session id: {error}"),
            })
        })?;
    let created_at_ms = row
        .try_get::<i64, _>("created_at_ms")
        .map_err(|error| session_sql_error("decode link created_at_ms", error))
        .and_then(|value| {
            i64_to_u64(value, "link created_at_ms")
                .map_err(|message| SessionStoreError::Store { message })
        })?;
    Ok(SessionLinkRecord {
        from_session_id,
        to_session_id,
        relationship: row
            .try_get("relationship")
            .map_err(|error| session_sql_error("decode link relationship", error))?,
        created_at_ms,
        metadata: row
            .try_get("metadata")
            .map_err(|error| session_sql_error("decode link metadata", error))?,
    })
}

async fn lock_session(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    session_id: &SessionId,
    action: &'static str,
) -> Result<SessionRecord, SessionStoreError> {
    let query = format!(
        r#"
        SELECT {SESSION_COLUMNS}
        FROM sessions
        WHERE universe_id = $1 AND session_id = $2
        FOR UPDATE
        "#,
    );
    let row = sqlx::query(&query)
        .bind(universe_id)
        .bind(session_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| session_sql_error(action, error))?;
    let Some(row) = row else {
        return Err(SessionStoreError::SessionNotFound {
            session_id: session_id.clone(),
        });
    };
    session_record_from_row(&row)
}

async fn copy_session_resources_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    source_session_id: &SessionId,
    child_session_id: &SessionId,
) -> Result<(), SessionStoreError> {
    sqlx::query(
        r#"
        INSERT INTO vfs_mounts (
            universe_id,
            session_id,
            mount_path,
            source_kind,
            snapshot_digest,
            workspace_id,
            access
        )
        SELECT
            universe_id,
            $3,
            mount_path,
            source_kind,
            snapshot_digest,
            workspace_id,
            access
        FROM vfs_mounts
        WHERE universe_id = $1 AND session_id = $2
        ON CONFLICT (universe_id, session_id, mount_path) DO UPDATE
        SET
            source_kind = EXCLUDED.source_kind,
            snapshot_digest = EXCLUDED.snapshot_digest,
            workspace_id = EXCLUDED.workspace_id,
            access = EXCLUDED.access
        "#,
    )
    .bind(universe_id)
    .bind(source_session_id.as_str())
    .bind(child_session_id.as_str())
    .execute(&mut **tx)
    .await
    .map_err(|error| session_sql_error("copy vfs mounts", error))?;

    Ok(())
}

async fn append_events_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    mut record: SessionRecord,
    events: Vec<UncommittedStoredEvent>,
) -> Result<(SessionRecord, Vec<StoredSessionEntry>), SessionStoreError> {
    let mut committed = Vec::with_capacity(events.len());
    for event in events {
        let next_seq = EventSeq::new(
            record
                .head
                .as_ref()
                .map_or(1, |position| position.seq.as_u64().saturating_add(1)),
        );
        let position = SessionPosition { seq: next_seq };
        let entry = StoredSessionEntry {
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
        .bind(universe_id)
        .bind(record.session_id.as_str())
        .bind(entry_json)
        .execute(&mut **tx)
        .await
        .map_err(|error| session_sql_error("insert session event", error))?;
        record.head = Some(position);
        record.updated_at_ms = entry.observed_at_ms;
        committed.push(entry);
    }

    if let Some(last) = committed.last() {
        sqlx::query(
            r#"
            UPDATE sessions
            SET head_seq = $3, updated_at_ms = $4
            WHERE universe_id = $1 AND session_id = $2
            "#,
        )
        .bind(universe_id)
        .bind(record.session_id.as_str())
        .bind(event_seq_to_i64(last.position.seq)?)
        .bind(u64_to_i64(last.observed_at_ms, "updated_at_ms")?)
        .execute(&mut **tx)
        .await
        .map_err(|error| session_sql_error("update session head", error))?;
    }

    Ok((record, committed))
}
