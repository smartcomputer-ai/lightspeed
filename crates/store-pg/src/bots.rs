//! PostgreSQL storage for the bots registry: bots, triggers, and the per-bot
//! event log. Bot decisions live in the controller's Temporal history; these
//! tables are the read model plus what admission needs (the `#N` counter,
//! trigger secrets and incidents, write-once outcomes).

use ::bots::{
    BotError, BotEventCursor, BotEventOutcomeWrite, BotEventRateScope, BotEventRecord,
    BotEventStore, BotRecord, BotRosterRow, BotStore, BotTriggerRecord, BotTriggerStore,
    BotTriggerWrite, InsertBotEventOutcome,
    validate::{validate_bot_document, validate_trigger_document},
};
use api::{
    BotDocument, BotEventOutcome, BotId, BotTriggerDisabledReason, BotTriggerId, BotTriggerKind,
    PollCursorState, ProfileId,
};
use async_trait::async_trait;
use sqlx::Row;

use crate::PgStore;

const BOT_COLUMNS: &str = r#"
    bot_id, revision, document_json, event_seq, closed_at_ms, closed_sessions_json,
    created_at_ms, updated_at_ms
"#;

const TRIGGER_COLUMNS: &str = r#"
    bot_id, trigger_id, kind, revision, document_json, secrets_json,
    disabled_reason, disabled_at_ms, last_filter_error, last_filter_error_at_ms,
    cursor_json, created_at_ms, updated_at_ms
"#;

const EVENT_COLUMN_NAMES: &[&str] = &[
    "bot_id",
    "event_id",
    "seq",
    "trigger_id",
    "kind",
    "summary",
    "occurred_at_ms",
    "received_at_ms",
    "document_ref",
    "prompt_ref",
    "session_json",
    "media_json",
    "sender_bot_id",
    "hops",
    "in_reply_to_json",
    "receiver_json",
    "outcome",
    "outcome_detail",
    "run_id",
    "resolved_at_ms",
];

/// Column prefix of the roster's lateral last-event columns.
const ROSTER_EVENT_PREFIX: &str = "last_event_";

fn event_columns() -> String {
    EVENT_COLUMN_NAMES.join(", ")
}

/// `table.col AS <prefix>col, ...` for embedding the event columns beside
/// other tables' columns.
fn prefixed_event_columns(table: &str, prefix: &str) -> String {
    EVENT_COLUMN_NAMES
        .iter()
        .map(|column| format!("{table}.{column} AS {prefix}{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ── Bots ────────────────────────────────────────────────────────────────────

impl PgStore {
    /// The roster for one reader: only bots the reader may read.
    pub async fn list_bot_roster_for(
        &self,
        reader: &crate::Reader,
    ) -> Result<Vec<BotRosterRow>, BotError> {
        let last_event_columns = prefixed_event_columns("le", ROSTER_EVENT_PREFIX);
        let readable = crate::resources::readable_predicate(reader, "bot", "b", "bot_id", 2);
        let event_columns = event_columns();
        let query = format!(
            r#"
            SELECT
                b.bot_id, b.revision, b.document_json, b.event_seq, b.closed_at_ms,
                b.closed_sessions_json, b.created_at_ms, b.updated_at_ms,
                tc.trigger_count,
                pc.pending_count,
                {last_event_columns}
            FROM bots b
            LEFT JOIN LATERAL (
                SELECT count(*) AS trigger_count
                FROM bot_triggers t
                WHERE t.universe_id = b.universe_id AND t.bot_id = b.bot_id
            ) tc ON true
            LEFT JOIN LATERAL (
                SELECT count(*) AS pending_count
                FROM bot_events p
                WHERE p.universe_id = b.universe_id AND p.bot_id = b.bot_id
                  AND p.outcome IS NULL
            ) pc ON true
            LEFT JOIN LATERAL (
                SELECT {event_columns}
                FROM bot_events e
                WHERE e.universe_id = b.universe_id AND e.bot_id = b.bot_id
                ORDER BY e.received_at_ms DESC, e.seq DESC
                LIMIT 1
            ) le ON true
            WHERE b.universe_id = $1 AND {readable}
            ORDER BY b.bot_id
            "#
        );
        let (reader_principal, reader_root_kind, reader_root_id) = reader.binds();
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(reader_principal)
            .bind(reader_root_kind)
            .bind(reader_root_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("list bot roster", error))?;
        rows.iter()
            .map(|row| {
                let bot = bot_from_row(row)?;
                let trigger_count: i64 = row
                    .try_get("trigger_count")
                    .map_err(|error| bot_sql_error("decode trigger count", error))?;
                let pending_count: i64 = row
                    .try_get("pending_count")
                    .map_err(|error| bot_sql_error("decode pending count", error))?;
                let last_event_id: Option<String> = row
                    .try_get(format!("{ROSTER_EVENT_PREFIX}event_id").as_str())
                    .map_err(|error| bot_sql_error("decode last event id", error))?;
                let last_event = match last_event_id {
                    Some(_) => Some(event_from_row(row, ROSTER_EVENT_PREFIX)?),
                    None => None,
                };
                Ok(BotRosterRow {
                    bot,
                    trigger_count: u32::try_from(trigger_count).unwrap_or(u32::MAX),
                    pending_count: u64::try_from(pending_count).unwrap_or(0),
                    last_event,
                })
            })
            .collect()
    }
}

#[async_trait]
impl BotStore for PgStore {
    async fn create_bot(
        &self,
        bot_id: BotId,
        document: BotDocument,
        now_ms: i64,
    ) -> Result<BotRecord, BotError> {
        self.ensure_universe()
            .await
            .map_err(|error| bot_store_error("ensure universe", error))?;
        validate_bot_document(&document)?;
        let query = format!(
            r#"
            INSERT INTO bots (
                universe_id, bot_id, revision, document_json, event_seq,
                closed_at_ms, closed_sessions_json, created_at_ms, updated_at_ms
            )
            VALUES ($1, $2, 1, $3, 0, NULL, '[]'::jsonb, $4, $4)
            ON CONFLICT (universe_id, bot_id) DO NOTHING
            RETURNING {BOT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(json_value("serialize bot document", &document)?)
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("create bot", error))?;
        let Some(row) = row else {
            return Err(BotError::BotAlreadyExists { bot_id });
        };
        bot_from_row(&row)
    }

    async fn put_bot(
        &self,
        bot_id: BotId,
        document: BotDocument,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<BotRecord, BotError> {
        self.ensure_universe()
            .await
            .map_err(|error| bot_store_error("ensure universe", error))?;
        validate_bot_document(&document)?;
        // A concurrent writer between the read and the write loses exactly one
        // retry; the recheck still enforces `expected_revision` against fresh
        // state, so the retry never bypasses the caller's guard.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let current = match self.read_bot(&bot_id).await {
                Ok(current) => Some(current),
                Err(BotError::BotNotFound { .. }) => None,
                Err(error) => return Err(error),
            };
            let Some(current) = current else {
                match self
                    .create_bot(bot_id.clone(), document.clone(), now_ms)
                    .await
                {
                    Ok(created) => return Ok(created),
                    Err(BotError::BotAlreadyExists { .. }) if attempt < 2 => continue,
                    Err(error) => return Err(error),
                }
            };
            if let Some(expected) = expected_revision
                && current.revision != expected
            {
                return Err(BotError::BotRevisionConflict {
                    bot_id,
                    expected,
                    actual: current.revision,
                });
            }
            if current.is_closed() {
                // A closed bot keeps its record for history; only the human
                // labels may still change.
                let mut labels_only = document.clone();
                labels_only.display_name = current.document.display_name.clone();
                labels_only.description = current.document.description.clone();
                if labels_only != current.document {
                    return Err(BotError::BotClosed { bot_id });
                }
            }
            let guard_revision = current.revision;
            let next_revision = guard_revision
                .checked_add(1)
                .ok_or_else(|| BotError::store(format!("bot {bot_id} revision overflow")))?;
            match self
                .cas_write_bot(&bot_id, &document, next_revision, now_ms, guard_revision)
                .await?
            {
                Some(written) => return Ok(written),
                None if attempt < 2 => continue,
                None => {
                    let actual = self.read_bot(&bot_id).await?.revision;
                    return Err(BotError::BotRevisionConflict {
                        bot_id,
                        expected: guard_revision,
                        actual,
                    });
                }
            }
        }
    }

    async fn read_bot(&self, bot_id: &BotId) -> Result<BotRecord, BotError> {
        let query =
            format!("SELECT {BOT_COLUMNS} FROM bots WHERE universe_id = $1 AND bot_id = $2");
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("read bot", error))?;
        let Some(row) = row else {
            return Err(BotError::BotNotFound {
                bot_id: bot_id.clone(),
            });
        };
        bot_from_row(&row)
    }

    async fn list_bots(&self) -> Result<Vec<BotRecord>, BotError> {
        let query =
            format!("SELECT {BOT_COLUMNS} FROM bots WHERE universe_id = $1 ORDER BY bot_id");
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("list bots", error))?;
        rows.iter().map(bot_from_row).collect()
    }

    async fn list_bot_roster(&self) -> Result<Vec<BotRosterRow>, BotError> {
        self.list_bot_roster_for(&crate::Reader::Everything).await
    }

    async fn list_bots_for_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<Vec<BotRecord>, BotError> {
        let query = format!(
            r#"
            SELECT {BOT_COLUMNS} FROM bots
            WHERE universe_id = $1
              AND closed_at_ms IS NULL
              AND document_json->>'profileId' = $2
            ORDER BY bot_id
            "#
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(profile_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("list bots for profile", error))?;
        rows.iter().map(bot_from_row).collect()
    }

    async fn close_bot(&self, bot_id: &BotId, now_ms: i64) -> Result<BotRecord, BotError> {
        let query = format!(
            r#"
            UPDATE bots SET
                closed_at_ms = $3,
                document_json = jsonb_set(document_json, '{{enabled}}', 'false'::jsonb),
                revision = revision + 1,
                updated_at_ms = $3
            WHERE universe_id = $1 AND bot_id = $2 AND closed_at_ms IS NULL
            RETURNING {BOT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("close bot", error))?;
        match row {
            Some(row) => bot_from_row(&row),
            // Already closed (returned unchanged) or absent (`BotNotFound`).
            None => self.read_bot(bot_id).await,
        }
    }

    async fn record_bot_closed_sessions(
        &self,
        bot_id: &BotId,
        sessions: Vec<String>,
    ) -> Result<Vec<String>, BotError> {
        // Union in one statement: append, keep the first occurrence of each
        // id, and restore the original order.
        let row = sqlx::query(
            r#"
            UPDATE bots SET closed_sessions_json = (
                SELECT COALESCE(jsonb_agg(value ORDER BY ord), '[]'::jsonb)
                FROM (
                    SELECT DISTINCT ON (value) value, ord
                    FROM jsonb_array_elements(closed_sessions_json || $3::jsonb)
                        WITH ORDINALITY AS entries (value, ord)
                    ORDER BY value, ord
                ) AS first_occurrences
            )
            WHERE universe_id = $1 AND bot_id = $2
            RETURNING closed_sessions_json
            "#,
        )
        .bind(self.config.universe_id)
        .bind(bot_id.as_str())
        .bind(json_value("serialize closed sessions", &sessions)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| bot_sql_error("record bot closed sessions", error))?;
        let Some(row) = row else {
            return Err(BotError::BotNotFound {
                bot_id: bot_id.clone(),
            });
        };
        json_column(&row, "closed_sessions_json")
    }

    async fn delete_bot(&self, bot_id: &BotId) -> Result<BotRecord, BotError> {
        let query = format!(
            "DELETE FROM bots WHERE universe_id = $1 AND bot_id = $2 RETURNING {BOT_COLUMNS}"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("delete bot", error))?;
        let Some(row) = row else {
            return Err(BotError::BotNotFound {
                bot_id: bot_id.clone(),
            });
        };
        // The anchor goes with the bot, so the id is free again.
        sqlx::query("DELETE FROM access_resources WHERE universe_id = $1 AND resource_kind = 'bot' AND resource_id = $2")
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|error| bot_sql_error("release bot anchor", error))?;
        bot_from_row(&row)
    }

    async fn allocate_bot_event_seq(&self, bot_id: &BotId) -> Result<u64, BotError> {
        let seq: Option<i64> = sqlx::query_scalar(
            r#"
            UPDATE bots SET event_seq = event_seq + 1
            WHERE universe_id = $1 AND bot_id = $2
            RETURNING event_seq
            "#,
        )
        .bind(self.config.universe_id)
        .bind(bot_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| bot_sql_error("allocate bot event seq", error))?;
        let Some(seq) = seq else {
            return Err(BotError::BotNotFound {
                bot_id: bot_id.clone(),
            });
        };
        i64_to_u64(seq, "event_seq")
    }
}

impl PgStore {
    /// Write `document` at `revision` over the row currently at
    /// `guard_revision`. Returns `None` when the guard no longer matches (a
    /// concurrent writer won).
    async fn cas_write_bot(
        &self,
        bot_id: &BotId,
        document: &BotDocument,
        revision: u64,
        now_ms: i64,
        guard_revision: u64,
    ) -> Result<Option<BotRecord>, BotError> {
        let query = format!(
            r#"
            UPDATE bots SET
                revision = $3,
                document_json = $4,
                updated_at_ms = $5
            WHERE universe_id = $1 AND bot_id = $2 AND revision = $6
            RETURNING {BOT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(u64_to_i64(revision, "revision")?)
            .bind(json_value("serialize bot document", document)?)
            .bind(now_ms)
            .bind(u64_to_i64(guard_revision, "revision")?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("write bot", error))?;
        row.as_ref().map(bot_from_row).transpose()
    }
}

// ── Triggers ────────────────────────────────────────────────────────────────

#[async_trait]
impl BotTriggerStore for PgStore {
    async fn put_bot_trigger(
        &self,
        bot_id: &BotId,
        write: BotTriggerWrite,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<BotTriggerRecord, BotError> {
        validate_trigger_document(&write.document, now_ms)?;
        // Same optimistic-retry shape as `put_bot`.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let current = match self.read_bot_trigger(bot_id, &write.trigger_id).await {
                Ok(current) => Some(current),
                Err(BotError::TriggerNotFound { .. }) => None,
                Err(error) => return Err(error),
            };
            let Some(current) = current else {
                match self.insert_trigger(bot_id, &write, now_ms).await {
                    Ok(Some(created)) => return Ok(created),
                    Ok(None) if attempt < 2 => continue,
                    Ok(None) => {
                        return Err(BotError::store(format!(
                            "bot trigger {bot_id}/{} changed concurrently",
                            write.trigger_id
                        )));
                    }
                    Err(error) => return Err(error),
                }
            };
            if let Some(expected) = expected_revision
                && current.revision != expected
            {
                return Err(BotError::TriggerRevisionConflict {
                    bot_id: bot_id.clone(),
                    trigger_id: write.trigger_id,
                    expected,
                    actual: current.revision,
                });
            }
            let guard_revision = current.revision;
            let next_revision = guard_revision.checked_add(1).ok_or_else(|| {
                BotError::store(format!(
                    "bot trigger {bot_id}/{} revision overflow",
                    write.trigger_id
                ))
            })?;
            let cursor = match &write.cursor {
                Some(override_cursor) => override_cursor.clone(),
                None => current.cursor.clone(),
            };
            // A replaced trigger keeps its incidents unless the document
            // enables it again, which clears the runtime disable.
            let (disabled_reason, disabled_at_ms) = if write.document.enabled {
                (None, None)
            } else {
                (current.disabled_reason, current.disabled_at_ms)
            };
            let replaced = BotTriggerRecord {
                bot_id: bot_id.clone(),
                trigger_id: write.trigger_id.clone(),
                revision: next_revision,
                document: write.document.clone(),
                secrets: write.secrets.clone(),
                disabled_reason,
                disabled_at_ms,
                last_filter_error: current.last_filter_error.clone(),
                last_filter_error_at_ms: current.last_filter_error_at_ms,
                cursor,
                created_at_ms: current.created_at_ms,
                updated_at_ms: now_ms,
            };
            match self.cas_write_trigger(&replaced, guard_revision).await? {
                Some(written) => return Ok(written),
                None if attempt < 2 => continue,
                None => {
                    let actual = self
                        .read_bot_trigger(bot_id, &write.trigger_id)
                        .await?
                        .revision;
                    return Err(BotError::TriggerRevisionConflict {
                        bot_id: bot_id.clone(),
                        trigger_id: write.trigger_id,
                        expected: guard_revision,
                        actual,
                    });
                }
            }
        }
    }

    async fn read_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, BotError> {
        let query = format!(
            "SELECT {TRIGGER_COLUMNS} FROM bot_triggers \
             WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(trigger_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("read bot trigger", error))?;
        let Some(row) = row else {
            return Err(BotError::TriggerNotFound {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            });
        };
        trigger_from_row(&row)
    }

    async fn list_bot_triggers(&self, bot_id: &BotId) -> Result<Vec<BotTriggerRecord>, BotError> {
        let query = format!(
            "SELECT {TRIGGER_COLUMNS} FROM bot_triggers \
             WHERE universe_id = $1 AND bot_id = $2 ORDER BY trigger_id"
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("list bot triggers", error))?;
        rows.iter().map(trigger_from_row).collect()
    }

    async fn list_bot_triggers_by_kind(
        &self,
        kind: BotTriggerKind,
    ) -> Result<Vec<BotTriggerRecord>, BotError> {
        let query = format!(
            "SELECT {TRIGGER_COLUMNS} FROM bot_triggers \
             WHERE universe_id = $1 AND kind = $2 ORDER BY bot_id, trigger_id"
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(kind.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("list bot triggers by kind", error))?;
        rows.iter().map(trigger_from_row).collect()
    }

    async fn delete_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> Result<BotTriggerRecord, BotError> {
        let query = format!(
            "DELETE FROM bot_triggers \
             WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3 \
             RETURNING {TRIGGER_COLUMNS}"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(trigger_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("delete bot trigger", error))?;
        let Some(row) = row else {
            return Err(BotError::TriggerNotFound {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            });
        };
        trigger_from_row(&row)
    }

    async fn disable_bot_trigger(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        reason: BotTriggerDisabledReason,
        now_ms: i64,
    ) -> Result<BotTriggerRecord, BotError> {
        let query = format!(
            r#"
            UPDATE bot_triggers SET
                document_json = jsonb_set(document_json, '{{enabled}}', 'false'::jsonb),
                disabled_reason = $4,
                disabled_at_ms = $5,
                revision = revision + 1,
                updated_at_ms = $5
            WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3
            RETURNING {TRIGGER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(trigger_id.as_str())
            .bind(disabled_reason_to_str(reason))
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("disable bot trigger", error))?;
        let Some(row) = row else {
            return Err(BotError::TriggerNotFound {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            });
        };
        trigger_from_row(&row)
    }

    async fn disable_bot_triggers(
        &self,
        bot_id: &BotId,
        reason: BotTriggerDisabledReason,
        now_ms: i64,
    ) -> Result<Vec<BotTriggerRecord>, BotError> {
        let query = format!(
            r#"
            UPDATE bot_triggers SET
                document_json = jsonb_set(document_json, '{{enabled}}', 'false'::jsonb),
                disabled_reason = $3,
                disabled_at_ms = $4,
                revision = revision + 1,
                updated_at_ms = $4
            WHERE universe_id = $1 AND bot_id = $2
              AND COALESCE((document_json->>'enabled')::boolean, true)
            RETURNING {TRIGGER_COLUMNS}
            "#
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(disabled_reason_to_str(reason))
            .bind(now_ms)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("disable bot triggers", error))?;
        let mut changed = rows
            .iter()
            .map(trigger_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        changed.sort_by(|left, right| left.trigger_id.cmp(&right.trigger_id));
        Ok(changed)
    }

    async fn set_bot_trigger_filter_error(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        error: Option<String>,
        now_ms: i64,
    ) -> Result<(), BotError> {
        let at_ms = error.as_ref().map(|_| now_ms);
        let result = sqlx::query(
            r#"
            UPDATE bot_triggers SET
                last_filter_error = $4,
                last_filter_error_at_ms = $5
            WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3
            "#,
        )
        .bind(self.config.universe_id)
        .bind(bot_id.as_str())
        .bind(trigger_id.as_str())
        .bind(error.as_deref())
        .bind(at_ms)
        .execute(&self.pool)
        .await
        .map_err(|error| bot_sql_error("set bot trigger filter error", error))?;
        if result.rows_affected() == 0 {
            return Err(BotError::TriggerNotFound {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            });
        }
        Ok(())
    }

    async fn set_bot_trigger_cursor(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
        cursor: Option<PollCursorState>,
    ) -> Result<(), BotError> {
        let cursor_json = cursor
            .as_ref()
            .map(|cursor| json_value("serialize poll cursor", cursor))
            .transpose()?;
        let result = sqlx::query(
            r#"
            UPDATE bot_triggers SET cursor_json = $4
            WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3
            "#,
        )
        .bind(self.config.universe_id)
        .bind(bot_id.as_str())
        .bind(trigger_id.as_str())
        .bind(cursor_json)
        .execute(&self.pool)
        .await
        .map_err(|error| bot_sql_error("set bot trigger cursor", error))?;
        if result.rows_affected() == 0 {
            return Err(BotError::TriggerNotFound {
                bot_id: bot_id.clone(),
                trigger_id: trigger_id.clone(),
            });
        }
        Ok(())
    }
}

impl PgStore {
    /// Insert a fresh trigger at revision 1. `Ok(None)` when the id is taken
    /// (the caller re-reads and replaces).
    async fn insert_trigger(
        &self,
        bot_id: &BotId,
        write: &BotTriggerWrite,
        now_ms: i64,
    ) -> Result<Option<BotTriggerRecord>, BotError> {
        let cursor_json = write
            .cursor
            .as_ref()
            .and_then(Option::as_ref)
            .map(|cursor| json_value("serialize poll cursor", cursor))
            .transpose()?;
        let query = format!(
            r#"
            INSERT INTO bot_triggers (
                universe_id, bot_id, trigger_id, kind, revision, document_json, secrets_json,
                disabled_reason, disabled_at_ms, last_filter_error, last_filter_error_at_ms,
                cursor_json, created_at_ms, updated_at_ms
            )
            VALUES ($1, $2, $3, $4, 1, $5, $6, NULL, NULL, NULL, NULL, $7, $8, $8)
            ON CONFLICT (universe_id, bot_id, trigger_id) DO NOTHING
            RETURNING {TRIGGER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(write.trigger_id.as_str())
            .bind(write.document.spec.kind().as_str())
            .bind(json_value("serialize trigger document", &write.document)?)
            .bind(json_value("serialize trigger secrets", &write.secrets)?)
            .bind(cursor_json)
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| map_trigger_write_error(bot_id, "insert bot trigger", error))?;
        row.as_ref().map(trigger_from_row).transpose()
    }

    /// Write `replaced` over the row currently at `guard_revision`. Returns
    /// `None` when the guard no longer matches (a concurrent writer won).
    async fn cas_write_trigger(
        &self,
        replaced: &BotTriggerRecord,
        guard_revision: u64,
    ) -> Result<Option<BotTriggerRecord>, BotError> {
        let cursor_json = replaced
            .cursor
            .as_ref()
            .map(|cursor| json_value("serialize poll cursor", cursor))
            .transpose()?;
        let query = format!(
            r#"
            UPDATE bot_triggers SET
                kind = $4,
                revision = $5,
                document_json = $6,
                secrets_json = $7,
                disabled_reason = $8,
                disabled_at_ms = $9,
                cursor_json = $10,
                updated_at_ms = $11
            WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3 AND revision = $12
            RETURNING {TRIGGER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(replaced.bot_id.as_str())
            .bind(replaced.trigger_id.as_str())
            .bind(replaced.kind().as_str())
            .bind(u64_to_i64(replaced.revision, "revision")?)
            .bind(json_value(
                "serialize trigger document",
                &replaced.document,
            )?)
            .bind(json_value("serialize trigger secrets", &replaced.secrets)?)
            .bind(replaced.disabled_reason.map(disabled_reason_to_str))
            .bind(replaced.disabled_at_ms)
            .bind(cursor_json)
            .bind(replaced.updated_at_ms)
            .bind(u64_to_i64(guard_revision, "revision")?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                map_trigger_write_error(&replaced.bot_id, "write bot trigger", error)
            })?;
        row.as_ref().map(trigger_from_row).transpose()
    }
}

// ── Events ──────────────────────────────────────────────────────────────────

#[async_trait]
impl BotEventStore for PgStore {
    async fn insert_bot_event(
        &self,
        record: BotEventRecord,
    ) -> Result<InsertBotEventOutcome, BotError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| BotError::store(format!("begin bot event insertion: {error}")))?;
        let columns = event_columns();
        let query = format!(
            r#"
            INSERT INTO bot_events (
                universe_id, bot_id, event_id, seq,
                trigger_id, kind, summary, occurred_at_ms, received_at_ms, document_ref,
                prompt_ref, session_json, media_json,
                sender_bot_id, hops, in_reply_to_json,
                receiver_json,
                outcome, outcome_detail, run_id, resolved_at_ms
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                $18, $19, $20, $21
            )
            ON CONFLICT (universe_id, bot_id, event_id) DO NOTHING
            RETURNING {columns}
            "#
        );
        let row =
            sqlx::query(&query)
                .bind(self.config.universe_id)
                .bind(record.bot_id.as_str())
                .bind(record.event_id.as_str())
                .bind(u64_to_i64(record.seq, "seq")?)
                .bind(record.trigger_id.as_ref().map(BotTriggerId::as_str))
                .bind(record.kind.as_str())
                .bind(record.summary.as_str())
                .bind(record.occurred_at_ms)
                .bind(record.received_at_ms)
                .bind(record.document_ref.as_str())
                .bind(record.prompt_ref.as_deref())
                .bind(
                    record
                        .session
                        .as_ref()
                        .map(|session| json_value("serialize routed session", session))
                        .transpose()?,
                )
                .bind(json_value("serialize event media", &record.media)?)
                .bind(record.sender_bot_id.as_ref().map(BotId::as_str))
                .bind(i32::try_from(record.hops).map_err(|_| {
                    BotError::invalid(format!("hops {} exceeds i32::MAX", record.hops))
                })?)
                .bind(
                    record
                        .in_reply_to
                        .as_ref()
                        .map(|reply| json_value("serialize in-reply-to", reply))
                        .transpose()?,
                )
                .bind(
                    record
                        .receiver
                        .as_ref()
                        .map(|receiver| json_value("serialize event receiver", receiver))
                        .transpose()?,
                )
                .bind(record.outcome.map(BotEventOutcome::as_str))
                .bind(record.outcome_detail.as_deref())
                .bind(record.run_id.as_deref())
                .bind(record.resolved_at_ms)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_event_insert_error(&record.bot_id, error))?;
        if let Some(row) = row {
            let mut refs = std::collections::BTreeSet::from([record.document_ref.clone()]);
            refs.extend(record.prompt_ref.iter().cloned());
            refs.extend(record.media.iter().map(|media| media.blob_ref.clone()));
            refs.extend(
                record
                    .receiver
                    .as_ref()
                    .and_then(|receiver| receiver.tools_ref())
                    .map(str::to_owned),
            );
            let digests = refs
                .iter()
                .map(|value| {
                    engine::BlobRef::parse(value)
                        .map(|blob| blob.as_str()[7..].to_owned())
                        .map_err(|error| {
                            BotError::invalid(format!("invalid bot event blob ref: {error}"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            sqlx::query(
                "INSERT INTO cas_bot_event_roots (universe_id, bot_id, event_id, digest) \
                 SELECT $1, $2, $3, digest FROM unnest($4::text[]) AS r(digest) \
                 ORDER BY digest ON CONFLICT DO NOTHING",
            )
            .bind(self.config.universe_id)
            .bind(record.bot_id.as_str())
            .bind(record.event_id.as_str())
            .bind(digests)
            .execute(&mut *tx)
            .await
            .map_err(|error| BotError::store(format!("record bot event blob roots: {error}")))?;
            tx.commit()
                .await
                .map_err(|error| BotError::store(format!("commit bot event insertion: {error}")))?;
            return Ok(InsertBotEventOutcome::Inserted(event_from_row(&row, "")?));
        }
        tx.commit()
            .await
            .map_err(|error| BotError::store(format!("commit duplicate bot event: {error}")))?;
        // The id was already stored: hand back the stored row so `#N` stays
        // stable. A delete racing in between surfaces as not found.
        self.read_bot_event(&record.bot_id, &record.event_id)
            .await
            .map(InsertBotEventOutcome::Duplicate)
    }

    async fn delete_bot_event(&self, bot_id: &BotId, event_id: &str) -> Result<bool, BotError> {
        let result = sqlx::query(
            "DELETE FROM bot_events WHERE universe_id = $1 AND bot_id = $2 AND event_id = $3",
        )
        .bind(self.config.universe_id)
        .bind(bot_id.as_str())
        .bind(event_id)
        .execute(&self.pool)
        .await
        .map_err(|error| bot_sql_error("delete bot event", error))?;
        Ok(result.rows_affected() > 0)
    }

    async fn read_bot_event_by_seq(
        &self,
        bot_id: &BotId,
        seq: u64,
    ) -> Result<BotEventRecord, BotError> {
        let columns = event_columns();
        let query = format!(
            "SELECT {columns} FROM bot_events WHERE universe_id = $1 AND bot_id = $2 AND seq = $3"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(u64_to_i64(seq, "seq")?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("read bot event by seq", error))?;
        let Some(row) = row else {
            return Err(BotError::EventNotFound {
                bot_id: bot_id.clone(),
                seq,
            });
        };
        event_from_row(&row, "")
    }

    async fn read_bot_event(
        &self,
        bot_id: &BotId,
        event_id: &str,
    ) -> Result<BotEventRecord, BotError> {
        let columns = event_columns();
        let query = format!(
            "SELECT {columns} FROM bot_events \
             WHERE universe_id = $1 AND bot_id = $2 AND event_id = $3"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| bot_sql_error("read bot event", error))?;
        let Some(row) = row else {
            return Err(BotError::EventIdNotFound {
                bot_id: bot_id.clone(),
                event_id: event_id.to_owned(),
            });
        };
        event_from_row(&row, "")
    }

    async fn read_bot_events(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
    ) -> Result<Vec<BotEventRecord>, BotError> {
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }
        let columns = event_columns();
        let query = format!(
            "SELECT {columns} FROM bot_events \
             WHERE universe_id = $1 AND bot_id = $2 AND event_id = ANY($3::text[]) \
             ORDER BY seq"
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(bot_id.as_str())
            .bind(event_ids.to_vec())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| bot_sql_error("read bot events", error))?;
        rows.iter().map(|row| event_from_row(row, "")).collect()
    }

    async fn list_bot_events(
        &self,
        bot_id: &BotId,
        limit: usize,
        before: Option<BotEventCursor>,
    ) -> Result<Vec<BotEventRecord>, BotError> {
        let columns = event_columns();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = match before {
            Some(cursor) => {
                let query = format!(
                    "SELECT {columns} FROM bot_events \
                     WHERE universe_id = $1 AND bot_id = $2 \
                       AND (received_at_ms, seq) < ($3, $4) \
                     ORDER BY received_at_ms DESC, seq DESC LIMIT $5"
                );
                sqlx::query(&query)
                    .bind(self.config.universe_id)
                    .bind(bot_id.as_str())
                    .bind(cursor.received_at_ms)
                    .bind(u64_to_i64(cursor.seq, "seq")?)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                let query = format!(
                    "SELECT {columns} FROM bot_events \
                     WHERE universe_id = $1 AND bot_id = $2 \
                     ORDER BY received_at_ms DESC, seq DESC LIMIT $3"
                );
                sqlx::query(&query)
                    .bind(self.config.universe_id)
                    .bind(bot_id.as_str())
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(|error| bot_sql_error("list bot events", error))?;
        rows.iter().map(|row| event_from_row(row, "")).collect()
    }

    async fn count_bot_events_since(
        &self,
        scope: BotEventRateScope<'_>,
        since_ms: i64,
    ) -> Result<u64, BotError> {
        let count: i64 = match scope {
            BotEventRateScope::Trigger { bot_id, trigger_id } => {
                sqlx::query_scalar(
                    "SELECT count(*) FROM bot_events \
                     WHERE universe_id = $1 AND bot_id = $2 AND trigger_id = $3 \
                       AND received_at_ms >= $4",
                )
                .bind(self.config.universe_id)
                .bind(bot_id.as_str())
                .bind(trigger_id.as_str())
                .bind(since_ms)
                .fetch_one(&self.pool)
                .await
            }
            BotEventRateScope::Sender { sender_bot_id } => {
                sqlx::query_scalar(
                    "SELECT count(*) FROM bot_events \
                     WHERE universe_id = $1 AND sender_bot_id = $2 \
                       AND received_at_ms >= $3",
                )
                .bind(self.config.universe_id)
                .bind(sender_bot_id.as_str())
                .bind(since_ms)
                .fetch_one(&self.pool)
                .await
            }
        }
        .map_err(|error| bot_sql_error("count bot events", error))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn record_bot_event_outcomes(
        &self,
        bot_id: &BotId,
        event_ids: &[String],
        write: BotEventOutcomeWrite,
    ) -> Result<u64, BotError> {
        if event_ids.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query(
            r#"
            UPDATE bot_events SET
                outcome = $4,
                outcome_detail = $5,
                run_id = $6,
                resolved_at_ms = $7
            WHERE universe_id = $1 AND bot_id = $2 AND event_id = ANY($3::text[])
              AND outcome IS NULL
            "#,
        )
        .bind(self.config.universe_id)
        .bind(bot_id.as_str())
        .bind(event_ids.to_vec())
        .bind(write.outcome.as_str())
        .bind(write.detail.as_deref())
        .bind(write.run_id.as_deref())
        .bind(write.resolved_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|error| bot_sql_error("record bot event outcomes", error))?;
        Ok(result.rows_affected())
    }
}

// ── Row decoding ────────────────────────────────────────────────────────────

fn bot_from_row(row: &sqlx::postgres::PgRow) -> Result<BotRecord, BotError> {
    let bot_id: String = column(row, "bot_id")?;
    let revision: i64 = column(row, "revision")?;
    let event_seq: i64 = column(row, "event_seq")?;
    Ok(BotRecord {
        bot_id: parse_bot_id(bot_id)?,
        revision: i64_to_u64(revision, "revision")?,
        document: json_column(row, "document_json")?,
        event_seq: i64_to_u64(event_seq, "event_seq")?,
        closed_at_ms: column(row, "closed_at_ms")?,
        closed_sessions: json_column(row, "closed_sessions_json")?,
        created_at_ms: column(row, "created_at_ms")?,
        updated_at_ms: column(row, "updated_at_ms")?,
    })
}

fn trigger_from_row(row: &sqlx::postgres::PgRow) -> Result<BotTriggerRecord, BotError> {
    let bot_id: String = column(row, "bot_id")?;
    let trigger_id: String = column(row, "trigger_id")?;
    let revision: i64 = column(row, "revision")?;
    let disabled_reason: Option<String> = column(row, "disabled_reason")?;
    Ok(BotTriggerRecord {
        bot_id: parse_bot_id(bot_id)?,
        trigger_id: parse_trigger_id(trigger_id)?,
        revision: i64_to_u64(revision, "revision")?,
        document: json_column(row, "document_json")?,
        secrets: json_column(row, "secrets_json")?,
        disabled_reason: disabled_reason
            .as_deref()
            .map(disabled_reason_from_str)
            .transpose()?,
        disabled_at_ms: column(row, "disabled_at_ms")?,
        last_filter_error: column(row, "last_filter_error")?,
        last_filter_error_at_ms: column(row, "last_filter_error_at_ms")?,
        cursor: optional_json_column(row, "cursor_json")?,
        created_at_ms: column(row, "created_at_ms")?,
        updated_at_ms: column(row, "updated_at_ms")?,
    })
}

/// Decode an event whose columns carry `prefix` (empty for a plain select).
fn event_from_row(row: &sqlx::postgres::PgRow, prefix: &str) -> Result<BotEventRecord, BotError> {
    let name = |column: &str| format!("{prefix}{column}");
    let bot_id: String = column(row, &name("bot_id"))?;
    let seq: i64 = column(row, &name("seq"))?;
    let trigger_id: Option<String> = column(row, &name("trigger_id"))?;
    let sender_bot_id: Option<String> = column(row, &name("sender_bot_id"))?;
    let hops: i32 = column(row, &name("hops"))?;
    let outcome: Option<String> = column(row, &name("outcome"))?;
    Ok(BotEventRecord {
        bot_id: parse_bot_id(bot_id)?,
        event_id: column(row, &name("event_id"))?,
        seq: i64_to_u64(seq, "seq")?,
        trigger_id: trigger_id.map(parse_trigger_id).transpose()?,
        kind: column(row, &name("kind"))?,
        summary: column(row, &name("summary"))?,
        occurred_at_ms: column(row, &name("occurred_at_ms"))?,
        received_at_ms: column(row, &name("received_at_ms"))?,
        document_ref: column(row, &name("document_ref"))?,
        prompt_ref: column(row, &name("prompt_ref"))?,
        session: optional_json_column(row, &name("session_json"))?,
        media: json_column(row, &name("media_json"))?,
        sender_bot_id: sender_bot_id.map(parse_bot_id).transpose()?,
        hops: u32::try_from(hops).map_err(|_| store_message("hops is negative"))?,
        in_reply_to: optional_json_column(row, &name("in_reply_to_json"))?,
        receiver: optional_json_column(row, &name("receiver_json"))?,
        outcome: outcome.as_deref().map(outcome_from_str).transpose()?,
        outcome_detail: column(row, &name("outcome_detail"))?,
        run_id: column(row, &name("run_id"))?,
        resolved_at_ms: column(row, &name("resolved_at_ms"))?,
    })
}

fn parse_bot_id(value: String) -> Result<BotId, BotError> {
    BotId::try_new(value).map_err(|error| store_message(format!("decode bot id: {error}")))
}

fn parse_trigger_id(value: String) -> Result<BotTriggerId, BotError> {
    BotTriggerId::try_new(value)
        .map_err(|error| store_message(format!("decode trigger id: {error}")))
}

fn disabled_reason_to_str(value: BotTriggerDisabledReason) -> &'static str {
    match value {
        BotTriggerDisabledReason::Breaker => "breaker",
        BotTriggerDisabledReason::PollFailed => "poll_failed",
        BotTriggerDisabledReason::OneShot => "one_shot",
        BotTriggerDisabledReason::Operator => "operator",
        BotTriggerDisabledReason::BotClosed => "bot_closed",
    }
}

fn disabled_reason_from_str(value: &str) -> Result<BotTriggerDisabledReason, BotError> {
    match value {
        "breaker" => Ok(BotTriggerDisabledReason::Breaker),
        "poll_failed" => Ok(BotTriggerDisabledReason::PollFailed),
        "one_shot" => Ok(BotTriggerDisabledReason::OneShot),
        "operator" => Ok(BotTriggerDisabledReason::Operator),
        "bot_closed" => Ok(BotTriggerDisabledReason::BotClosed),
        other => Err(store_message(format!(
            "unsupported trigger disabled reason '{other}'"
        ))),
    }
}

fn outcome_from_str(value: &str) -> Result<BotEventOutcome, BotError> {
    match value {
        "handled" => Ok(BotEventOutcome::Handled),
        "deferred" => Ok(BotEventOutcome::Deferred),
        "ignored" => Ok(BotEventOutcome::Ignored),
        "blocked" => Ok(BotEventOutcome::Blocked),
        "unresolved" => Ok(BotEventOutcome::Unresolved),
        "run_failed" => Ok(BotEventOutcome::RunFailed),
        "steered" => Ok(BotEventOutcome::Steered),
        "appended" => Ok(BotEventOutcome::Appended),
        "archived" => Ok(BotEventOutcome::Archived),
        other => Err(store_message(format!(
            "unsupported bot event outcome '{other}'"
        ))),
    }
}

fn column<'r, T>(row: &'r sqlx::postgres::PgRow, name: &str) -> Result<T, BotError>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(name)
        .map_err(|error| bot_sql_error(&format!("decode column {name}"), error))
}

fn json_value<T: serde::Serialize>(action: &str, value: &T) -> Result<serde_json::Value, BotError> {
    serde_json::to_value(value).map_err(|error| store_message(format!("{action}: {error}")))
}

fn json_column<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<T, BotError> {
    let value: serde_json::Value = column(row, name)?;
    serde_json::from_value(value).map_err(|error| store_message(format!("decode {name}: {error}")))
}

fn optional_json_column<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<Option<T>, BotError> {
    let value: Option<serde_json::Value> = column(row, name)?;
    value
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| store_message(format!("decode {name}: {error}")))
}

fn u64_to_i64(value: u64, name: &str) -> Result<i64, BotError> {
    i64::try_from(value).map_err(|_| BotError::invalid(format!("{name} exceeds i64::MAX")))
}

fn i64_to_u64(value: i64, name: &str) -> Result<u64, BotError> {
    u64::try_from(value).map_err(|_| store_message(format!("{name} is negative")))
}

fn constraint_name(error: &sqlx::Error) -> Option<&str> {
    error.as_database_error().and_then(|db| db.constraint())
}

fn map_trigger_write_error(bot_id: &BotId, action: &str, error: sqlx::Error) -> BotError {
    match constraint_name(&error) {
        Some("bot_triggers_bot_fk") => BotError::BotNotFound {
            bot_id: bot_id.clone(),
        },
        Some("bot_triggers_inbox_unique_idx") => {
            BotError::invalid(format!("bot {bot_id} already has an inbox trigger"))
        }
        _ => bot_sql_error(action, error),
    }
}

fn map_event_insert_error(bot_id: &BotId, error: sqlx::Error) -> BotError {
    match constraint_name(&error) {
        Some("bot_events_bot_fk") => BotError::BotNotFound {
            bot_id: bot_id.clone(),
        },
        Some("bot_events_seq_unique") => {
            BotError::invalid(format!("bot {bot_id} event seq is already stored"))
        }
        _ => bot_sql_error("insert bot event", error),
    }
}

fn bot_store_error(action: &str, error: crate::PgStoreError) -> BotError {
    store_message(format!("{action}: {error}"))
}

fn bot_sql_error(action: &str, error: sqlx::Error) -> BotError {
    store_message(format!("{action}: {error}"))
}

fn store_message(message: impl Into<String>) -> BotError {
    BotError::Store {
        message: message.into(),
    }
}
