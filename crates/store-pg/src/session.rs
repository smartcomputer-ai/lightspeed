use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use engine::{
    BlobRef,
    session::{EventSeq, SessionId, SessionPosition, StoredSessionEntry, UncommittedStoredEvent},
    storage::{
        AdvanceSessionCheckpoint, AppendSessionEvents, AppendSessionEventsResult,
        CreateClonedSession, CreateForkedSession, CreateSession, DeleteClosedSessions,
        DeleteClosedSessionsResult, ListSessions, ReadSessionEventRange, ReadSessionEvents,
        SessionCheckpoint, SessionLifecycleStatus, SessionListCursor, SessionListPage,
        SessionOrigin, SessionOriginCounts, SessionPage, SessionRecord, SessionStore,
        SessionStoreError, apply_lifecycle_projection, check_origin_limits, collect_blob_refs,
        collect_content_refs, largest_safe_fork_seq, lifecycle_at_fork, validate_fork_point,
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
    metadata_json,
    lifecycle_status,
    closed_at_seq,
    closed_at_ms,
    retention_root_session_id,
    delete_after_close_ms,
    delete_at_ms,
    managed,
    head_seq,
    source_session_id,
    source_seq,
    origin_json,
    origin_root_session_id,
    origin_parent_session_id,
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
        let mut embedded = EmbeddedBlobRefs::default();
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
            embedded.observe(&entry_json);
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
        embedded
            .record_roots(&mut tx, self.config.universe_id, &request.session_id)
            .await?;

        if let Some(last) = committed.last() {
            record.updated_at_ms = last.observed_at_ms;
            for entry in &committed {
                apply_lifecycle_projection(&mut record, entry);
            }
            sqlx::query(
                r#"
                UPDATE sessions
                SET head_seq = $3,
                    updated_at_ms = $4,
                    lifecycle_status = $5,
                    closed_at_seq = $6,
                    managed = $7,
                    closed_at_ms = $8
                WHERE universe_id = $1 AND session_id = $2
                "#,
            )
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(event_seq_to_i64(last.position.seq)?)
            .bind(u64_to_i64(last.observed_at_ms, "updated_at_ms")?)
            .bind(session_lifecycle_status_str(record.lifecycle_status))
            .bind(optional_event_seq_to_i64(
                record.closed_at_seq,
                "closed_at_seq",
            )?)
            .bind(record.managed)
            .bind(optional_u64_to_i64(record.closed_at_ms, "closed_at_ms")?)
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
        let record = self.load_session(session_id).await?.ok_or_else(|| {
            SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            }
        })?;
        let through = record.head.as_ref().map_or(0, |head| head.seq.as_u64());
        self.read_effective_window_through(session_id, after, through, limit)
            .await
    }

    async fn read_effective_window_through(
        &self,
        session_id: &SessionId,
        after: u64,
        through: u64,
        limit: usize,
    ) -> Result<Vec<StoredSessionEntry>, SessionStoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let segments = self.resolve_segments(session_id, through).await?;
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

impl PgStore {
    /// `list_sessions` for one reader: the page holds only sessions the
    /// reader may read, decided in SQL by the resource policy, each with the
    /// access summary its view carries.
    pub async fn list_sessions_for(
        &self,
        request: ListSessions,
        reader: &crate::Reader,
    ) -> Result<crate::SessionListPageWithAccess, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        let fetch_limit = usize_to_session_i64(request.limit.saturating_add(1), "limit")?;
        let (cursor_updated_at_ms, cursor_session_id) = match &request.cursor {
            Some(cursor) => (
                Some(u64_to_i64(cursor.updated_at_ms, "cursor updated_at_ms")?),
                Some(cursor.session_id.as_str().to_owned()),
            ),
            None => (None, None),
        };
        // Metadata predicates are appended only when their filter form is
        // present. Exact containment can use the GIN index; presence matching
        // uses PostgreSQL's native all-keys operator. A `$n IS NULL OR` form
        // would force weaker generic plans.
        let metadata_exact = request
            .metadata
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let metadata_keys = request
            .metadata
            .iter()
            .filter(|(_, value)| value.is_empty())
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let metadata_filter = (!metadata_exact.is_empty())
            .then(|| metadata_json(&metadata_exact))
            .transpose()?;
        let metadata_predicate = match (metadata_filter.is_some(), metadata_keys.is_empty()) {
            (true, false) => "AND sessions.metadata_json @> $10 AND sessions.metadata_json ?& $11",
            (true, true) => "AND sessions.metadata_json @> $10",
            (false, false) => "AND sessions.metadata_json ?& $10",
            (false, true) => "",
        };
        let lifecycle_predicate = if request.exclude_closed {
            "AND sessions.lifecycle_status <> 'closed'"
        } else {
            ""
        };
        let crate::resources::ReadableClauses {
            filter: readable,
            privileged,
        } = crate::resources::readable_clauses(reader, "session", "sessions", "session_id", 7);
        let (reader_principal, reader_root_kind, reader_root_id) = reader.binds();
        let summary_columns = crate::resources::SUMMARY_COLUMNS;
        // The anchor join brings columns of the same names; qualify ours.
        let session_columns = SESSION_COLUMNS
            .split(',')
            .map(str::trim)
            .filter(|column| !column.is_empty())
            .map(|column| format!("sessions.{column}"))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"
            SELECT {session_columns}, {summary_columns}, {privileged} AS access_privileged
            FROM sessions
            JOIN access_resources ra ON ra.universe_id = sessions.universe_id AND ra.resource_kind = 'session' AND ra.resource_id = sessions.session_id
            JOIN access_resource_policies rp ON rp.universe_id = ra.universe_id AND rp.resource_kind = ra.audience_root_kind AND rp.resource_id = ra.audience_root_id
            WHERE sessions.universe_id = $1
              AND ($2::bigint IS NULL OR (sessions.updated_at_ms, sessions.session_id) < ($2, $3))
              AND ($4::text IS NULL OR sessions.origin_root_session_id = $4)
              AND ($5::text IS NULL OR sessions.origin_parent_session_id = $5)
              {lifecycle_predicate}
              AND {readable}
              {metadata_predicate}
            ORDER BY sessions.updated_at_ms DESC, sessions.session_id DESC
            LIMIT $6
            "#,
        );
        let mut sql = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(cursor_updated_at_ms)
            .bind(cursor_session_id)
            .bind(
                request
                    .root_session_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
            )
            .bind(
                request
                    .parent_session_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
            )
            .bind(fetch_limit)
            .bind(reader_principal)
            .bind(reader_root_kind)
            .bind(reader_root_id);
        if let Some(filter) = metadata_filter {
            sql = sql.bind(filter);
        }
        if !metadata_keys.is_empty() {
            sql = sql.bind(metadata_keys);
        }
        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| session_sql_error("list sessions", error))?;

        let store_error = |error: ::access::AccessError| SessionStoreError::Store {
            message: error.to_string(),
        };
        let mut sessions = rows
            .iter()
            .map(|row| {
                Ok((
                    session_record_from_row(row)?,
                    crate::resources::summary_from_list_row(row).map_err(store_error)?,
                    crate::resources::privileged_from_list_row(row).map_err(store_error)?,
                ))
            })
            .collect::<Result<Vec<_>, SessionStoreError>>()?;
        let next_cursor = (sessions.len() > request.limit).then(|| {
            sessions.truncate(request.limit);
            let (last, _, _) = sessions.last().expect("non-empty page");
            SessionListCursor {
                updated_at_ms: last.updated_at_ms,
                session_id: last.session_id.clone(),
            }
        });
        // Only the returned page counts as read.
        let privileged = sessions.iter().any(|(_, _, privileged)| *privileged);
        Ok(crate::SessionListPageWithAccess {
            sessions: sessions
                .into_iter()
                .map(|(record, access, _)| (record, access))
                .collect(),
            next_cursor,
            privileged,
        })
    }
}

#[async_trait]
impl SessionStore for PgStore {
    async fn create_session(
        &self,
        request: CreateSession,
    ) -> Result<SessionRecord, SessionStoreError> {
        if request.delete_after_close_ms == Some(0) {
            return Err(SessionStoreError::InvalidRetention {
                message: "deleteAfterCloseMs must be positive".to_owned(),
            });
        }
        self.ensure_universe()
            .await
            .map_err(|error| session_store_error("ensure universe", error))?;
        let created_at_ms = u64_to_i64(request.created_at_ms, "created_at_ms")?;
        let origin_parent = match request.origin.as_ref() {
            Some(origin) => Some(
                self.load_session(&origin.parent_session_id)
                    .await?
                    .ok_or_else(|| SessionStoreError::SessionNotFound {
                        session_id: origin.parent_session_id.clone(),
                    })?,
            ),
            None => None,
        };
        let retention_root_session_id = origin_parent.as_ref().map_or_else(
            || request.session_id.clone(),
            |parent| parent.retention_root_session_id.clone(),
        );
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| session_sql_error("begin create session transaction", error))?;
        if let Some(origin) = &request.origin {
            if request.delete_after_close_ms.is_some() {
                return Err(SessionStoreError::SessionRetentionOwnedBy {
                    session_id: request.session_id,
                    retention_root_session_id: origin.root_session_id.clone(),
                });
            }
            // Lock retention ownership first, matching cascade deletion.
            // The child row is then the root-limit reservation: count and
            // insert in this transaction so concurrent spawns serialize.
            lock_session(
                &mut tx,
                self.config.universe_id,
                &retention_root_session_id,
                "lock retention root for child creation",
            )
            .await?;
            if origin.root_session_id != retention_root_session_id {
                lock_session(
                    &mut tx,
                    self.config.universe_id,
                    &origin.root_session_id,
                    "lock subagent root for reservation",
                )
                .await?;
            }
            if origin.parent_session_id != retention_root_session_id
                && origin.parent_session_id != origin.root_session_id
            {
                lock_session(
                    &mut tx,
                    self.config.universe_id,
                    &origin.parent_session_id,
                    "lock parent session for reservation",
                )
                .await?;
            }
            let counts =
                origin_counts_in_tx(&mut tx, self.config.universe_id, &origin.root_session_id)
                    .await?;
            check_origin_limits(origin, counts)?;
        }
        let origin_columns = OriginColumns::from_origin(request.origin.as_ref())?;
        let query = format!(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                display_name,
                origin_json,
                origin_root_session_id,
                origin_parent_session_id,
                retention_root_session_id,
                delete_after_close_ms,
                created_at_ms,
                updated_at_ms,
                metadata_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10)
            ON CONFLICT (universe_id, session_id) DO NOTHING
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(request.display_name.as_deref())
            .bind(origin_columns.json)
            .bind(origin_columns.root_session_id)
            .bind(origin_columns.parent_session_id)
            .bind(retention_root_session_id.as_str())
            .bind(optional_u64_to_i64(
                request.delete_after_close_ms,
                "delete_after_close_ms",
            )?)
            .bind(created_at_ms)
            .bind(metadata_json(&request.metadata)?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| session_sql_error("create session", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionAlreadyExists {
                session_id: request.session_id,
            });
        };
        let record = session_record_from_row(&row)?;
        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit create session", error))?;
        Ok(record)
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
        let page = self
            .list_sessions_for(request, &crate::Reader::Everything)
            .await?;
        Ok(SessionListPage {
            sessions: page
                .sessions
                .into_iter()
                .map(|(record, _)| record)
                .collect(),
            next_cursor: page.next_cursor,
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

    async fn set_session_metadata(
        &self,
        session_id: &SessionId,
        metadata: BTreeMap<String, String>,
    ) -> Result<SessionRecord, SessionStoreError> {
        let query = format!(
            r#"
            UPDATE sessions
            SET metadata_json = $3
            WHERE universe_id = $1 AND session_id = $2
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(metadata_json(&metadata)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| session_sql_error("set session metadata", error))?;

        let Some(row) = row else {
            return Err(SessionStoreError::SessionNotFound {
                session_id: session_id.clone(),
            });
        };
        session_record_from_row(&row)
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| session_sql_error("begin retention transaction", error))?;
        let record = lock_session(
            &mut tx,
            self.config.universe_id,
            session_id,
            "lock session for retention",
        )
        .await?;
        if record.retention_root_session_id != *session_id {
            return Err(SessionStoreError::SessionRetentionOwnedBy {
                session_id: session_id.clone(),
                retention_root_session_id: record.retention_root_session_id,
            });
        }
        let query = format!(
            r#"
            UPDATE sessions
            SET delete_after_close_ms = $3
            WHERE universe_id = $1 AND session_id = $2
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(optional_u64_to_i64(
                delete_after_close_ms,
                "delete_after_close_ms",
            )?)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| session_sql_error("set session retention", error))?;
        let record = session_record_from_row(&row)?;
        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit retention transaction", error))?;
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
        let query = format!(
            r#"
            SELECT {SESSION_COLUMNS}
            FROM sessions
            WHERE universe_id = $1
              AND retention_root_session_id = session_id
              AND lifecycle_status = 'closed'
              AND delete_at_ms IS NOT NULL
              AND delete_at_ms <= $2
            ORDER BY delete_at_ms, session_id
            LIMIT $3
            "#,
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(u64_to_i64(now_ms, "retention now_ms")?)
            .bind(usize_to_session_i64(limit, "retention limit")?)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| session_sql_error("list due retention roots", error))?;
        rows.iter().map(session_record_from_row).collect()
    }

    async fn delete_closed_sessions(
        &self,
        request: DeleteClosedSessions,
    ) -> Result<DeleteClosedSessionsResult, SessionStoreError> {
        let target_snapshot = self
            .load_session(&request.session_id)
            .await?
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: request.session_id.clone(),
            })?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| session_sql_error("begin delete session transaction", error))?;
        let root = lock_session(
            &mut tx,
            self.config.universe_id,
            &target_snapshot.retention_root_session_id,
            "lock retention root for delete",
        )
        .await?;
        let target = if target_snapshot.session_id == root.session_id {
            root
        } else {
            lock_session(
                &mut tx,
                self.config.universe_id,
                &request.session_id,
                "lock session for delete",
            )
            .await?
        };
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

        let session_ids: Vec<String> = sqlx::query_scalar(
            r#"
            WITH RECURSIVE tree(session_id) AS (
                SELECT $2::text
                UNION
                SELECT child.session_id
                FROM sessions AS child
                JOIN tree AS parent
                  ON (child.source_seq IS NOT NULL
                      AND child.source_session_id = parent.session_id)
                  OR child.origin_parent_session_id = parent.session_id
                WHERE child.universe_id = $1
            )
            SELECT session_id FROM tree ORDER BY session_id
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.session_id.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| session_sql_error("list session retention subtree", error))?;
        if !request.cascade && session_ids.len() > 1 {
            return Err(SessionStoreError::SessionHasChildren {
                session_id: request.session_id,
            });
        }
        let selected_ids = if request.cascade {
            session_ids
        } else {
            vec![request.session_id.as_str().to_owned()]
        };
        let query = format!(
            r#"
            SELECT {SESSION_COLUMNS}
            FROM sessions
            WHERE universe_id = $1 AND session_id = ANY($2)
            ORDER BY session_id
            FOR UPDATE
            "#,
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(&selected_ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| session_sql_error("lock session retention subtree", error))?;
        let records = rows
            .iter()
            .map(session_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        for record in &records {
            if record.lifecycle_status != SessionLifecycleStatus::Closed {
                let error = if record.session_id == request.session_id {
                    SessionStoreError::SessionNotClosed {
                        session_id: record.session_id.clone(),
                        lifecycle_status: record.lifecycle_status,
                    }
                } else {
                    SessionStoreError::SessionTreeNotClosed {
                        session_id: record.session_id.clone(),
                        lifecycle_status: record.lifecycle_status,
                    }
                };
                return Err(error);
            }
        }
        sqlx::query(
            r#"
            DELETE FROM sessions
            WHERE universe_id = $1 AND session_id = ANY($2)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(&selected_ids)
        .execute(&mut *tx)
        .await
        .map_err(|error| session_sql_error("delete session subtree", error))?;
        // The anchor goes with the content, so the id is free again.
        sqlx::query(
            "DELETE FROM access_resources WHERE universe_id = $1 AND resource_kind = 'session' AND resource_id = ANY($2)",
        )
        .bind(self.config.universe_id)
        .bind(&selected_ids)
        .execute(&mut *tx)
        .await
        .map_err(|error| session_sql_error("release session anchors", error))?;
        tx.commit()
            .await
            .map_err(|error| session_sql_error("commit delete session", error))?;
        let deleted_session_ids = selected_ids
            .into_iter()
            .map(SessionId::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SessionStoreError::Store {
                message: format!("decode deleted session id: {error}"),
            })?;
        Ok(DeleteClosedSessionsResult {
            target,
            deleted_session_ids,
        })
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

        let source = lock_session(
            &mut tx,
            self.config.universe_id,
            &request.source_session_id,
            "clone source",
        )
        .await?;
        if source.managed {
            return Err(SessionStoreError::ManagedSessionCannotBranch {
                session_id: request.source_session_id,
            });
        }
        let query = format!(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                source_session_id,
                source_seq,
                retention_root_session_id,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, NULL, $2, $4, $4)
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
        let source_record = self
            .load_session(&request.source_session_id)
            .await?
            .ok_or_else(|| SessionStoreError::SessionNotFound {
                session_id: request.source_session_id.clone(),
            })?;
        if source_record.managed {
            return Err(SessionStoreError::ManagedSessionCannotBranch {
                session_id: request.source_session_id,
            });
        }
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
        let retention_root = lock_session(
            &mut tx,
            self.config.universe_id,
            &source_record.retention_root_session_id,
            "lock retention root for fork creation",
        )
        .await?;
        let source = if source_record.session_id == retention_root.session_id {
            retention_root
        } else {
            lock_session(
                &mut tx,
                self.config.universe_id,
                &request.source_session_id,
                "fork source",
            )
            .await?
        };
        if source.managed {
            return Err(SessionStoreError::ManagedSessionCannotBranch {
                session_id: request.source_session_id,
            });
        }
        let (lifecycle_status, closed_at_seq) = lifecycle_at_fork(&source, request.source_seq);
        let closed_at_ms = closed_at_seq.and(source.closed_at_ms);
        let query = format!(
            r#"
            INSERT INTO sessions (
                universe_id,
                session_id,
                lifecycle_status,
                closed_at_seq,
                closed_at_ms,
                retention_root_session_id,
                managed,
                head_seq,
                source_session_id,
                source_seq,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)
            ON CONFLICT (universe_id, session_id) DO NOTHING
            RETURNING {SESSION_COLUMNS}
            "#,
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(session_lifecycle_status_str(lifecycle_status))
            .bind(optional_event_seq_to_i64(closed_at_seq, "closed_at_seq")?)
            .bind(optional_u64_to_i64(closed_at_ms, "closed_at_ms")?)
            .bind(source.retention_root_session_id.as_str())
            .bind(false)
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

    async fn read_range(
        &self,
        request: ReadSessionEventRange,
    ) -> Result<SessionPage, SessionStoreError> {
        if request.limit == 0 {
            return Err(SessionStoreError::InvalidLimit { limit: 0 });
        }
        if request.after >= request.through {
            return Ok(SessionPage {
                entries: Vec::new(),
                next_after: Some(request.after),
                complete: true,
            });
        }
        let mut selected = self
            .read_effective_window_through(
                &request.session_id,
                request.after.as_u64(),
                request.through.as_u64(),
                request.limit.saturating_add(1),
            )
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

    async fn load_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionCheckpoint>, SessionStoreError> {
        let row = sqlx::query(
            r#"
            SELECT through_seq, format_version, state_digest,
                   lineage_source_session_id, lineage_source_seq,
                   byte_len, created_at_ms
            FROM session_checkpoints
            WHERE universe_id = $1 AND session_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(session_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| session_sql_error("load session checkpoint", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let state_digest: String = row
            .try_get("state_digest")
            .map_err(|error| session_sql_error("decode checkpoint digest", error))?;
        let state_ref =
            engine::BlobRef::parse(format!("sha256:{state_digest}")).map_err(|error| {
                SessionStoreError::Store {
                    message: format!("decode checkpoint blob ref: {error}"),
                }
            })?;
        let through_seq: i64 = row
            .try_get("through_seq")
            .map_err(|error| session_sql_error("decode checkpoint sequence", error))?;
        let format_version: i32 = row
            .try_get("format_version")
            .map_err(|error| session_sql_error("decode checkpoint format", error))?;
        let lineage_source_session_id = row
            .try_get::<Option<String>, _>("lineage_source_session_id")
            .map_err(|error| session_sql_error("decode checkpoint lineage session", error))?
            .map(SessionId::try_new)
            .transpose()
            .map_err(|error| SessionStoreError::Store {
                message: format!("decode checkpoint lineage session: {error}"),
            })?;
        let lineage_source_seq = row
            .try_get::<Option<i64>, _>("lineage_source_seq")
            .map_err(|error| session_sql_error("decode checkpoint lineage sequence", error))?
            .map(|seq| i64_to_u64(seq, "checkpoint lineage sequence").map(EventSeq::new))
            .transpose()
            .map_err(|message| SessionStoreError::Store { message })?;
        let byte_len: i64 = row
            .try_get("byte_len")
            .map_err(|error| session_sql_error("decode checkpoint byte length", error))?;
        let created_at_ms: i64 = row
            .try_get("created_at_ms")
            .map_err(|error| session_sql_error("decode checkpoint created time", error))?;
        Ok(Some(SessionCheckpoint {
            session_id: session_id.clone(),
            through_seq: EventSeq::new(
                i64_to_u64(through_seq, "checkpoint sequence")
                    .map_err(|message| SessionStoreError::Store { message })?,
            ),
            format_version: u32::try_from(format_version).map_err(|_| {
                SessionStoreError::Store {
                    message: format!("checkpoint format version is invalid: {format_version}"),
                }
            })?,
            state_ref,
            lineage_source_session_id,
            lineage_source_seq,
            byte_len: i64_to_u64(byte_len, "checkpoint byte length")
                .map_err(|message| SessionStoreError::Store { message })?,
            created_at_ms: i64_to_u64(created_at_ms, "checkpoint created time")
                .map_err(|message| SessionStoreError::Store { message })?,
        }))
    }

    async fn advance_checkpoint(
        &self,
        request: AdvanceSessionCheckpoint,
    ) -> Result<bool, SessionStoreError> {
        let checkpoint = request.checkpoint;
        let digest = checkpoint
            .state_ref
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| SessionStoreError::Store {
                message: format!("unsupported checkpoint blob ref: {}", checkpoint.state_ref),
            })?;
        let result = sqlx::query(
            r#"
            INSERT INTO session_checkpoints (
                universe_id, session_id, through_seq, format_version,
                state_digest, lineage_source_session_id, lineage_source_seq,
                byte_len, created_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (universe_id, session_id) DO UPDATE SET
                through_seq = excluded.through_seq,
                format_version = excluded.format_version,
                state_digest = excluded.state_digest,
                lineage_source_session_id = excluded.lineage_source_session_id,
                lineage_source_seq = excluded.lineage_source_seq,
                byte_len = excluded.byte_len,
                created_at_ms = excluded.created_at_ms
            WHERE session_checkpoints.through_seq < excluded.through_seq
            "#,
        )
        .bind(self.config.universe_id)
        .bind(checkpoint.session_id.as_str())
        .bind(event_seq_to_i64(checkpoint.through_seq)?)
        .bind(
            i32::try_from(checkpoint.format_version).map_err(|_| SessionStoreError::Store {
                message: "checkpoint format version exceeds Postgres integer".to_owned(),
            })?,
        )
        .bind(digest)
        .bind(
            checkpoint
                .lineage_source_session_id
                .as_ref()
                .map(SessionId::as_str),
        )
        .bind(
            checkpoint
                .lineage_source_seq
                .map(event_seq_to_i64)
                .transpose()?,
        )
        .bind(u64_to_i64(checkpoint.byte_len, "checkpoint byte length")?)
        .bind(u64_to_i64(
            checkpoint.created_at_ms,
            "checkpoint created time",
        )?)
        .execute(&self.pool)
        .await
        .map_err(|error| session_sql_error("advance session checkpoint", error))?;
        Ok(result.rows_affected() == 1)
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
    let metadata = row
        .try_get::<serde_json::Value, _>("metadata_json")
        .map_err(|error| session_sql_error("decode session metadata", error))
        .and_then(|value| {
            serde_json::from_value::<BTreeMap<String, String>>(value).map_err(|error| {
                SessionStoreError::Store {
                    message: format!("decode session metadata: {error}"),
                }
            })
        })?;
    let lifecycle_status = row
        .try_get::<String, _>("lifecycle_status")
        .map_err(|error| session_sql_error("decode session lifecycle status", error))
        .and_then(|value| session_lifecycle_status_from_str(&value))?;
    let closed_at_seq = row
        .try_get::<Option<i64>, _>("closed_at_seq")
        .map_err(|error| session_sql_error("decode closed_at_seq", error))
        .and_then(optional_event_seq_from_i64)?;
    let closed_at_ms = row
        .try_get::<Option<i64>, _>("closed_at_ms")
        .map_err(|error| session_sql_error("decode closed_at_ms", error))
        .and_then(|value| optional_i64_to_u64(value, "closed_at_ms"))?;
    let retention_root_session_id = row
        .try_get::<String, _>("retention_root_session_id")
        .map_err(|error| session_sql_error("decode retention root session id", error))
        .and_then(|value| {
            SessionId::parse(value).map_err(|error| SessionStoreError::Store {
                message: format!("decode retention root session id: {error}"),
            })
        })?;
    let delete_after_close_ms = row
        .try_get::<Option<i64>, _>("delete_after_close_ms")
        .map_err(|error| session_sql_error("decode delete_after_close_ms", error))
        .and_then(|value| optional_i64_to_u64(value, "delete_after_close_ms"))?;
    let delete_at_ms = row
        .try_get::<Option<i64>, _>("delete_at_ms")
        .map_err(|error| session_sql_error("decode delete_at_ms", error))
        .and_then(|value| optional_i64_to_u64(value, "delete_at_ms"))?;
    let managed = row
        .try_get::<bool, _>("managed")
        .map_err(|error| session_sql_error("decode managed", error))?;
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
    let origin = session_origin_from_row(row)?;

    Ok(SessionRecord {
        session_id,
        display_name,
        metadata,
        lifecycle_status,
        closed_at_seq,
        closed_at_ms,
        retention_root_session_id,
        delete_after_close_ms,
        delete_at_ms,
        managed,
        head,
        source_session_id,
        source_seq,
        origin,
        created_at_ms,
        updated_at_ms,
    })
}

/// Metadata maps travel as jsonb objects; the column keeps a
/// `jsonb_typeof = 'object'` check, so an encoding failure is a store bug.
fn metadata_json(
    metadata: &BTreeMap<String, String>,
) -> Result<serde_json::Value, SessionStoreError> {
    serde_json::to_value(metadata).map_err(|error| SessionStoreError::Store {
        message: format!("encode session metadata: {error}"),
    })
}

fn session_origin_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<Option<SessionOrigin>, SessionStoreError> {
    let origin_json = row
        .try_get::<Option<serde_json::Value>, _>("origin_json")
        .map_err(|error| session_sql_error("decode session origin", error))?;
    origin_json
        .map(|value| {
            serde_json::from_value::<SessionOrigin>(value).map_err(|error| {
                SessionStoreError::Store {
                    message: format!("decode session origin: {error}"),
                }
            })
        })
        .transpose()
}

/// Bind-ready projection of an optional origin: the whole document plus the
/// two denormalized keys the queries need; all `None` for a root.
struct OriginColumns {
    json: Option<serde_json::Value>,
    root_session_id: Option<String>,
    parent_session_id: Option<String>,
}

impl OriginColumns {
    fn from_origin(origin: Option<&SessionOrigin>) -> Result<Self, SessionStoreError> {
        let Some(origin) = origin else {
            return Ok(Self {
                json: None,
                root_session_id: None,
                parent_session_id: None,
            });
        };
        Ok(Self {
            json: Some(
                serde_json::to_value(origin).map_err(|error| SessionStoreError::Store {
                    message: format!("encode session origin: {error}"),
                })?,
            ),
            root_session_id: Some(origin.root_session_id.as_str().to_owned()),
            parent_session_id: Some(origin.parent_session_id.as_str().to_owned()),
        })
    }
}

async fn origin_counts_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    root_session_id: &SessionId,
) -> Result<SessionOriginCounts, SessionStoreError> {
    let row = sqlx::query(
        r#"
        SELECT
            count(*) AS descendants,
            count(*) FILTER (WHERE lifecycle_status <> 'closed') AS open_descendants
        FROM sessions
        WHERE universe_id = $1 AND origin_root_session_id = $2
        "#,
    )
    .bind(universe_id)
    .bind(root_session_id.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| session_sql_error("count session origin descendants", error))?;
    let descendants = row
        .try_get::<i64, _>("descendants")
        .map_err(|error| session_sql_error("decode descendant count", error))?;
    let open_descendants = row
        .try_get::<i64, _>("open_descendants")
        .map_err(|error| session_sql_error("decode open descendant count", error))?;
    Ok(SessionOriginCounts {
        descendants: u64::try_from(descendants).unwrap_or(u64::MAX),
        open_descendants: u64::try_from(open_descendants).unwrap_or(u64::MAX),
    })
}

fn session_lifecycle_status_str(status: SessionLifecycleStatus) -> &'static str {
    match status {
        SessionLifecycleStatus::New => "new",
        SessionLifecycleStatus::Open => "open",
        SessionLifecycleStatus::Closed => "closed",
    }
}

fn session_lifecycle_status_from_str(
    value: &str,
) -> Result<SessionLifecycleStatus, SessionStoreError> {
    match value {
        "new" => Ok(SessionLifecycleStatus::New),
        "open" => Ok(SessionLifecycleStatus::Open),
        "closed" => Ok(SessionLifecycleStatus::Closed),
        other => Err(SessionStoreError::Store {
            message: format!("decode session lifecycle status: unknown value {other:?}"),
        }),
    }
}

fn optional_event_seq_to_i64(
    seq: Option<EventSeq>,
    field: &'static str,
) -> Result<Option<i64>, SessionStoreError> {
    seq.map(|seq| u64_to_i64(seq.as_u64(), field)).transpose()
}

fn optional_u64_to_i64(value: Option<u64>, field: &str) -> Result<Option<i64>, SessionStoreError> {
    value.map(|value| u64_to_i64(value, field)).transpose()
}

fn optional_i64_to_u64(value: Option<i64>, field: &str) -> Result<Option<u64>, SessionStoreError> {
    value
        .map(|value| {
            i64_to_u64(value, field).map_err(|message| SessionStoreError::Store { message })
        })
        .transpose()
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

async fn append_events_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    mut record: SessionRecord,
    events: Vec<UncommittedStoredEvent>,
) -> Result<(SessionRecord, Vec<StoredSessionEntry>), SessionStoreError> {
    let mut committed = Vec::with_capacity(events.len());
    let mut embedded = EmbeddedBlobRefs::default();
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
        embedded.observe(&entry_json);
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
        apply_lifecycle_projection(&mut record, &entry);
        committed.push(entry);
    }
    embedded
        .record_roots(tx, universe_id, &record.session_id)
        .await?;

    if let Some(last) = committed.last() {
        sqlx::query(
            r#"
            UPDATE sessions
            SET head_seq = $3,
                updated_at_ms = $4,
                lifecycle_status = $5,
                closed_at_seq = $6,
                managed = $7,
                closed_at_ms = $8
            WHERE universe_id = $1 AND session_id = $2
            "#,
        )
        .bind(universe_id)
        .bind(record.session_id.as_str())
        .bind(event_seq_to_i64(last.position.seq)?)
        .bind(u64_to_i64(last.observed_at_ms, "updated_at_ms")?)
        .bind(session_lifecycle_status_str(record.lifecycle_status))
        .bind(optional_event_seq_to_i64(
            record.closed_at_seq,
            "closed_at_seq",
        )?)
        .bind(record.managed)
        .bind(optional_u64_to_i64(record.closed_at_ms, "closed_at_ms")?)
        .execute(&mut **tx)
        .await
        .map_err(|error| session_sql_error("update session head", error))?;
    }

    Ok((record, committed))
}

/// Distinct refs in an append. The store derives FK-backed roots inside the
/// append transaction; callers never register roots separately. Refs the
/// runtime placed in content positions become `content` roots, readable
/// through the session; every other ref is retained as `scan`.
#[derive(Default)]
struct EmbeddedBlobRefs {
    refs: BTreeSet<BlobRef>,
    content: BTreeSet<BlobRef>,
}

impl EmbeddedBlobRefs {
    fn observe(&mut self, entry_json: &serde_json::Value) {
        self.refs.extend(collect_blob_refs(entry_json));
        self.content.extend(collect_content_refs(entry_json));
    }

    async fn record_roots(
        self,
        tx: &mut Transaction<'_, Postgres>,
        universe_id: Uuid,
        session_id: &SessionId,
    ) -> Result<(), SessionStoreError> {
        if self.refs.is_empty() {
            return Ok(());
        }
        let candidates = self
            .refs
            .iter()
            .map(|blob_ref| blob_ref.as_str()[7..].to_owned())
            .collect::<Vec<_>>();
        let content = self
            .content
            .iter()
            .map(|blob_ref| blob_ref.as_str()[7..].to_owned())
            .collect::<Vec<_>>();
        let missing: Vec<String> = sqlx::query_scalar(
            r#"
            WITH requested AS (
                SELECT digest FROM unnest($3::text[]) AS candidate(digest)
                WHERE NOT EXISTS (
                    SELECT 1 FROM cas_session_roots r
                    WHERE r.universe_id = $1 AND r.session_id = $2
                      AND r.digest = candidate.digest
                )
            ), held AS MATERIALIZED (
                SELECT b.digest FROM cas_blobs b
                JOIN requested r ON r.digest = b.digest
                WHERE b.universe_id = $1
                ORDER BY b.digest
                FOR KEY SHARE OF b
            ), inserted AS (
                INSERT INTO cas_session_roots (universe_id, session_id, digest, origin)
                SELECT $1, $2, digest, CASE WHEN digest = ANY($4::text[]) THEN 'content' ELSE 'scan' END FROM held
                ON CONFLICT (universe_id, session_id, digest) DO UPDATE SET origin = 'content'
                WHERE EXCLUDED.origin = 'content' AND cas_session_roots.origin <> 'content'
            )
            SELECT 'sha256:' || digest FROM requested
            WHERE digest NOT IN (SELECT digest FROM held)
            ORDER BY digest
            "#,
        )
        .bind(universe_id)
        .bind(session_id.as_str())
        .bind(&candidates)
        .bind(&content)
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| session_sql_error("record event blob roots", error))?;
        if missing.is_empty() {
            return Ok(());
        }
        Err(SessionStoreError::MissingBlobs {
            session_id: session_id.clone(),
            blob_refs: missing
                .into_iter()
                .map(BlobRef::parse)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| SessionStoreError::Store {
                    message: format!("decode missing blob ref: {error}"),
                })?,
        })
    }
}
