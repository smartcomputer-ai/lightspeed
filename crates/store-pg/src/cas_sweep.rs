//! Content-addressed blob collection primitives.
//!
//! FK-backed roots and holders protect durable references against concurrent
//! deletion. Explicit incoming edges protect children; parent deletion exposes
//! them on a later pass. Pages bound rows examined, including live rows.
//! Objects have incarnation-specific keys and are removed after catalog commit.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use harness::{BlobRef, storage::BlobStoreError};
use object_store::{ObjectStoreExt, path::Path as ObjectPath};
use sqlx::{PgPool, Postgres, Row, Transaction, pool::PoolConnection};
use thiserror::Error;

use crate::{
    PgStore,
    shared::{blob_sql_error, i64_to_u64, sha256_hex, usize_to_blob_i64},
};

/// One catalog row eligible for deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CasSweepCandidate {
    pub blob_ref: BlobRef,
    pub byte_len: u64,
    /// Set for object-backed blobs; the object is deleted after the row.
    pub object_key: Option<String>,
}

#[derive(Debug, Error)]
pub enum CasSweepError {
    /// A concurrent attachment or an uncovered holder blocked deletion.
    /// Nothing was deleted; the sweeper reports it and moves on.
    #[error("blob deletion conflicts with holder constraint {constraint}: {message}")]
    HolderConflict { constraint: String, message: String },

    #[error(transparent)]
    Blob(#[from] BlobStoreError),
}

/// Outcome of deleting object-store keys; failures are counted, not fatal,
/// because the rows are already gone and the objects are unreachable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CasObjectDeletion {
    pub deleted: usize,
    pub failures: Vec<(String, String)>,
}

/// `b` is the `cas_blobs` row under consideration. `$1` is the universe,
/// `$2` the age cutoff, `$3` the pinned digests.
const DEAD_BLOB_PREDICATE: &str = r#"
    b.universe_id = $1
    AND b.touched_at_ms < $2
    AND b.digest <> ALL($3::text[])
    AND NOT EXISTS (
        SELECT 1 FROM cas_session_roots AS r
        WHERE r.universe_id = b.universe_id AND r.digest = b.digest
    )
    AND NOT EXISTS (
        SELECT 1 FROM session_checkpoints AS c
        WHERE c.universe_id = b.universe_id AND c.state_digest = b.digest
    )
    AND NOT EXISTS (
        SELECT 1 FROM vfs_snapshots AS s
        WHERE s.universe_id = b.universe_id AND s.digest = b.digest
    )
    AND NOT EXISTS (
        SELECT 1 FROM vfs_workspaces AS w
        WHERE w.universe_id = b.universe_id
          AND (w.head_snapshot_digest = b.digest OR w.base_snapshot_digest = b.digest)
    )
    AND NOT EXISTS (
        SELECT 1 FROM cas_bot_event_roots AS r
        WHERE r.universe_id = b.universe_id AND r.digest = b.digest
    )
    AND NOT EXISTS (
        SELECT 1 FROM cas_blob_edges AS g
        WHERE g.universe_id = b.universe_id AND g.child_digest = b.digest
    )
"#;

/// Position in the ordered catalog. A completed traversal wraps to the start,
/// revisiting formerly live blobs and children exposed by parent deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CasSweepCursor {
    pub touched_at_ms: u64,
    pub blob_ref: BlobRef,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CasSweepPage {
    pub candidates: Vec<CasSweepCandidate>,
    pub scanned: usize,
    pub next_cursor: Option<CasSweepCursor>,
}

/// One session-level advisory lock for background collection and manual passes.
/// No database transaction remains open while the worker sleeps. Closing the
/// connection on drop also releases leadership on cancellation or shutdown.
pub struct CasSweepLeader {
    connection: PoolConnection<Postgres>,
}

impl CasSweepLeader {
    pub async fn try_acquire(pool: &PgPool) -> Result<Option<Self>, sqlx::Error> {
        let mut connection = pool.acquire().await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(0x4c53_4341_535f_4743_i64)
            .fetch_one(&mut *connection)
            .await?;
        if !acquired {
            return Ok(None);
        }
        connection.close_on_drop();
        Ok(Some(Self { connection }))
    }

    pub async fn check(&mut self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1")
            .execute(&mut *self.connection)
            .await?;
        Ok(())
    }
}

impl PgStore {
    /// Candidates among the first `limit` old catalog rows. Use the paged
    /// method to make progress past live rows across repeated passes.
    pub async fn list_sweep_candidates(
        &self,
        cutoff_ms: u64,
        pinned: &[BlobRef],
        limit: usize,
    ) -> Result<Vec<CasSweepCandidate>, CasSweepError> {
        Ok(self
            .scan_sweep_candidates(cutoff_ms, pinned, None, limit)
            .await?
            .candidates)
    }

    pub async fn scan_sweep_candidates(
        &self,
        cutoff_ms: u64,
        pinned: &[BlobRef],
        after: Option<&CasSweepCursor>,
        limit: usize,
    ) -> Result<CasSweepPage, CasSweepError> {
        if limit == 0 {
            return Ok(CasSweepPage::default());
        }
        let query = format!(
            r#"
            WITH page AS MATERIALIZED (
                SELECT universe_id, digest, byte_len, object_key, touched_at_ms
                FROM cas_blobs
                WHERE universe_id = $1 AND touched_at_ms < $2
                  AND (touched_at_ms, digest) > ($5, $6)
                ORDER BY touched_at_ms, digest LIMIT $4
            )
            SELECT b.digest, b.byte_len, b.object_key, b.touched_at_ms,
                   ({DEAD_BLOB_PREDICATE}) AS dead
            FROM page AS b ORDER BY b.touched_at_ms, b.digest
        "#
        );
        let mut tx = sweep_transaction(&self.pool).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(cutoff_to_i64(cutoff_ms)?)
            .bind(digests(pinned)?)
            .bind(usize_to_blob_i64(limit, "sweep limit")?)
            .bind(
                after
                    .map(|cursor| cutoff_to_i64(cursor.touched_at_ms))
                    .transpose()?
                    .unwrap_or(-1),
            )
            .bind(
                after
                    .map(|cursor| sha256_hex(&cursor.blob_ref))
                    .transpose()?
                    .unwrap_or(""),
            )
            .fetch_all(&mut *tx)
            .await
            .map_err(|error| blob_sql_error("scan sweep page", error))?;
        tx.commit()
            .await
            .map_err(|error| blob_sql_error("commit sweep scan", error))?;
        let mut page = CasSweepPage {
            scanned: rows.len(),
            ..Default::default()
        };
        for row in &rows {
            let candidate = candidate_from_row(row)?;
            let touched: i64 = row
                .try_get("touched_at_ms")
                .map_err(|error| blob_sql_error("decode sweep cursor", error))?;
            page.next_cursor = Some(CasSweepCursor {
                touched_at_ms: i64_to_u64(touched, "sweep cursor")
                    .map_err(|message| BlobStoreError::Store { message })?,
                blob_ref: candidate.blob_ref.clone(),
            });
            if row
                .try_get::<bool, _>("dead")
                .map_err(|error| blob_sql_error("decode liveness", error))?
            {
                page.candidates.push(candidate);
            }
        }
        if rows.len() < limit {
            page.next_cursor = None;
        }
        Ok(page)
    }

    /// Delete the catalog rows of `candidates` that are still dead and still
    /// older than `cutoff_ms` at deletion time. Returns what was deleted so
    /// the caller can remove the objects and account for the bytes.
    pub async fn delete_dead_blobs(
        &self,
        candidates: &[BlobRef],
        cutoff_ms: u64,
        pinned: &[BlobRef],
    ) -> Result<Vec<CasSweepCandidate>, CasSweepError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let query = format!(
            r#"
            DELETE FROM cas_blobs AS b
            WHERE b.digest = ANY($4::text[])
              AND {DEAD_BLOB_PREDICATE}
            RETURNING b.digest, b.byte_len, b.object_key
            "#
        );
        let mut tx = sweep_transaction(&self.pool).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(cutoff_to_i64(cutoff_ms)?)
            .bind(digests(pinned)?)
            .bind(digests(candidates)?)
            .fetch_all(&mut *tx)
            .await
            .map_err(sweep_delete_error)?;
        tx.commit().await.map_err(sweep_delete_error)?;
        rows.iter().map(candidate_from_row).collect()
    }

    /// Remove object-store objects whose catalog rows are already gone. A
    /// missing object counts as deleted; other failures are collected.
    pub async fn delete_blob_objects(&self, keys: &[String]) -> CasObjectDeletion {
        let mut outcome = CasObjectDeletion::default();
        if keys.is_empty() {
            return outcome;
        }
        let Some(object_store) = self.object_store.as_ref() else {
            outcome.failures.extend(
                keys.iter()
                    .map(|key| (key.clone(), "no object store is configured".to_owned())),
            );
            return outcome;
        };
        for key in keys {
            match object_store.delete(&ObjectPath::from(key.as_str())).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => outcome.deleted += 1,
                Err(error) => outcome.failures.push((key.clone(), error.to_string())),
            }
        }
        outcome
    }
}

fn candidate_from_row(row: &sqlx::postgres::PgRow) -> Result<CasSweepCandidate, CasSweepError> {
    let digest: String = row
        .try_get("digest")
        .map_err(|error| blob_sql_error("decode sweep digest", error))?;
    let byte_len: i64 = row
        .try_get("byte_len")
        .map_err(|error| blob_sql_error("decode sweep byte length", error))?;
    let object_key: Option<String> = row
        .try_get("object_key")
        .map_err(|error| blob_sql_error("decode sweep object key", error))?;
    Ok(CasSweepCandidate {
        blob_ref: BlobRef::parse(format!("sha256:{digest}")).map_err(|error| {
            BlobStoreError::Store {
                message: format!("decode sweep blob ref: {error}"),
            }
        })?,
        byte_len: i64_to_u64(byte_len, "sweep byte length")
            .map_err(|message| BlobStoreError::Store { message })?,
        object_key,
    })
}

fn digests(blob_refs: &[BlobRef]) -> Result<Vec<String>, CasSweepError> {
    blob_refs
        .iter()
        .map(|blob_ref| sha256_hex(blob_ref).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn cutoff_to_i64(cutoff_ms: u64) -> Result<i64, CasSweepError> {
    i64::try_from(cutoff_ms)
        .map_err(|_| BlobStoreError::Store {
            message: format!("sweep cutoff is too large for Postgres bigint: {cutoff_ms}"),
        })
        .map_err(Into::into)
}

/// The constraint name of a foreign-key violation (SQLSTATE 23503), if that
/// is what the error is.
fn holder_constraint(error: &sqlx::Error) -> Option<String> {
    let database_error = error.as_database_error()?;
    (database_error.code().as_deref() == Some("23503")).then(|| {
        database_error
            .constraint()
            .unwrap_or("<unknown>")
            .to_owned()
    })
}

fn sweep_delete_error(error: sqlx::Error) -> CasSweepError {
    match holder_constraint(&error) {
        Some(constraint) => CasSweepError::HolderConflict {
            constraint,
            message: error.to_string(),
        },
        None => blob_sql_error("delete dead blobs", error).into(),
    }
}

/// Backstops for unexpectedly expensive plans or a concurrently held row.
async fn sweep_transaction(pool: &PgPool) -> Result<Transaction<'_, Postgres>, CasSweepError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| blob_sql_error("begin sweep transaction", error))?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await
        .map_err(|error| blob_sql_error("set sweep budget", error))?;
    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut *tx)
        .await
        .map_err(|error| blob_sql_error("set sweep lock budget", error))?;
    Ok(tx)
}
