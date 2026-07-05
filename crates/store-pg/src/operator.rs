//! Deployment-level universe administration.
//!
//! Everything here runs against the shared pool, above the universe-bound
//! [`PgStore`](crate::PgStore) boundary — the operator API addresses the set
//! of universes, so no per-universe store applies. Stats are cheap aggregates
//! computed at read time, approximate under concurrent writes by design.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::PgStoreError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniverseStats {
    pub universe_id: Uuid,
    pub slug: Option<String>,
    pub created_at_ms: i64,
    /// `max(sessions.updated_at_ms)`; `None` when the universe has no
    /// sessions.
    pub last_activity_at_ms: Option<i64>,
    pub sessions: u64,
    pub workspaces: u64,
    pub profiles: u64,
    pub blob_bytes: u64,
}

const UNIVERSE_STATS_SELECT: &str = r#"
    SELECT
        u.universe_id,
        u.slug,
        u.created_at_ms,
        (SELECT max(s.updated_at_ms) FROM sessions s
            WHERE s.universe_id = u.universe_id) AS last_activity_at_ms,
        (SELECT count(*) FROM sessions s
            WHERE s.universe_id = u.universe_id) AS sessions,
        (SELECT count(*) FROM vfs_workspaces w
            WHERE w.universe_id = u.universe_id) AS workspaces,
        (SELECT count(*) FROM agent_profiles p
            WHERE p.universe_id = u.universe_id) AS profiles,
        COALESCE((SELECT sum(b.byte_len) FROM cas_blobs b
            WHERE b.universe_id = u.universe_id), 0)::bigint AS blob_bytes
    FROM universes u
"#;

/// Create the universe row; `false` when it already existed.
pub async fn create_universe(pool: &PgPool, universe_id: Uuid) -> Result<bool, PgStoreError> {
    let result = sqlx::query(
        r#"
        INSERT INTO universes (universe_id)
        VALUES ($1)
        ON CONFLICT (universe_id) DO NOTHING
        "#,
    )
    .bind(universe_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn read_universe_stats(
    pool: &PgPool,
    universe_id: Uuid,
) -> Result<Option<UniverseStats>, PgStoreError> {
    let query = format!("{UNIVERSE_STATS_SELECT} WHERE u.universe_id = $1");
    let row = sqlx::query(&query)
        .bind(universe_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(universe_stats_from_row).transpose()
}

pub async fn list_universe_stats(pool: &PgPool) -> Result<Vec<UniverseStats>, PgStoreError> {
    let query = format!("{UNIVERSE_STATS_SELECT} ORDER BY u.created_at_ms, u.universe_id");
    let rows = sqlx::query(&query).fetch_all(pool).await?;
    rows.iter().map(universe_stats_from_row).collect()
}

/// Every session id in the universe — the purge terminates their workflows
/// before deleting rows.
pub async fn list_universe_session_ids(
    pool: &PgPool,
    universe_id: Uuid,
) -> Result<Vec<String>, PgStoreError> {
    let rows = sqlx::query(
        r#"
        SELECT session_id FROM sessions
        WHERE universe_id = $1
        ORDER BY session_id
        "#,
    )
    .bind(universe_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| row.try_get::<String, _>("session_id").map_err(Into::into))
        .collect()
}

/// Object-store keys of the universe's externally stored blobs. Deleting the
/// pg rows does not touch these bytes; the purge sweeps them explicitly.
pub async fn list_universe_object_keys(
    pool: &PgPool,
    universe_id: Uuid,
) -> Result<Vec<String>, PgStoreError> {
    let rows = sqlx::query(
        r#"
        SELECT object_key FROM cas_blobs
        WHERE universe_id = $1 AND storage_kind = 'object' AND object_key IS NOT NULL
        ORDER BY object_key
        "#,
    )
    .bind(universe_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| row.try_get::<String, _>("object_key").map_err(Into::into))
        .collect()
}

/// Delete the universe row; every universe-scoped table cascades from it.
/// `false` when the universe did not exist.
pub async fn delete_universe(pool: &PgPool, universe_id: Uuid) -> Result<bool, PgStoreError> {
    let result = sqlx::query("DELETE FROM universes WHERE universe_id = $1")
        .bind(universe_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

fn universe_stats_from_row(row: &sqlx::postgres::PgRow) -> Result<UniverseStats, PgStoreError> {
    Ok(UniverseStats {
        universe_id: row.try_get("universe_id")?,
        slug: row.try_get("slug")?,
        created_at_ms: row.try_get("created_at_ms")?,
        last_activity_at_ms: row.try_get("last_activity_at_ms")?,
        sessions: count_to_u64(row.try_get("sessions")?),
        workspaces: count_to_u64(row.try_get("workspaces")?),
        profiles: count_to_u64(row.try_get("profiles")?),
        blob_bytes: count_to_u64(row.try_get("blob_bytes")?),
    })
}

/// Counts and sums are non-negative by construction; clamp rather than fail.
fn count_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}
