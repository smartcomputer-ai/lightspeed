//! Deployment-level universe administration.
//!
//! Everything here runs against the shared pool, above the universe-bound
//! [`PgStore`](crate::PgStore) boundary — the deployment API addresses the set
//! of universes, so no per-universe store applies. Stats are cheap aggregates
//! computed at read time, approximate under concurrent writes by design.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::PgStoreError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniverseStats {
    pub configuration: api::UniverseConfiguration,
    pub universe_id: Uuid,
    pub handle: String,
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
        u.handle,
        u.display_name,
        u.platform_access,
        u.created_at_ms,
        (SELECT max(s.updated_at_ms) FROM sessions s
            WHERE s.universe_id = u.universe_id AND s.deleted_at_ms IS NULL) AS last_activity_at_ms,
        (SELECT count(*) FROM sessions s
            WHERE s.universe_id = u.universe_id AND s.deleted_at_ms IS NULL) AS sessions,
        (SELECT count(*) FROM vfs_workspaces w
            WHERE w.universe_id = u.universe_id) AS workspaces,
        (SELECT count(*) FROM agent_profiles p
            WHERE p.universe_id = u.universe_id) AS profiles,
        COALESCE((SELECT sum(b.byte_len) FROM cas_blobs b
            WHERE b.universe_id = u.universe_id), 0)::bigint AS blob_bytes
    FROM universes u
"#;

/// Longest universe handle: one DNS label.
pub const UNIVERSE_HANDLE_MAX_LEN: usize = 63;

/// Whether `handle` may name a universe: a lowercase DNS label that does not
/// read as a UUID, so a universe reference is always one or the other. The
/// `universes_handle_format` constraint enforces the same rule.
pub fn is_valid_universe_handle(handle: &str) -> bool {
    let bytes = handle.as_bytes();
    let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    !bytes.is_empty()
        && bytes.len() <= UNIVERSE_HANDLE_MAX_LEN
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && bytes.iter().all(|byte| edge(byte) || *byte == b'-')
        && Uuid::parse_str(handle).is_err()
}

/// The handle of a universe created without one. Derived from the UUID so a
/// repeated create names the universe the same way, and taken from its tail,
/// which is random in every UUID version that carries randomness.
pub fn default_universe_handle(universe_id: Uuid) -> String {
    let hex = universe_id.simple().to_string();
    format!("u-{}", &hex[hex.len() - 12..])
}

/// Create the universe row under its default handle; `false` when it already
/// existed.
pub async fn create_universe(pool: &PgPool, universe_id: Uuid) -> Result<bool, PgStoreError> {
    create_universe_with_handle(pool, universe_id, None).await
}

/// Create the universe row; `false` when it already existed, in which case
/// its handle is left as it was. `None` takes the default handle.
pub async fn create_universe_with_handle(
    pool: &PgPool,
    universe_id: Uuid,
    handle: Option<&str>,
) -> Result<bool, PgStoreError> {
    let result = sqlx::query(
        r#"
        INSERT INTO universes (universe_id, handle)
        VALUES ($1, $2)
        ON CONFLICT (universe_id) DO NOTHING
        "#,
    )
    .bind(universe_id)
    .bind(handle.map_or_else(|| default_universe_handle(universe_id), str::to_owned))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Rename a universe without changing its identity; the previous handle is
/// released. `false` when the universe does not exist.
pub async fn put_universe_handle(
    pool: &PgPool,
    universe_id: Uuid,
    handle: &str,
) -> Result<bool, PgStoreError> {
    let result = sqlx::query("UPDATE universes SET handle = $2 WHERE universe_id = $1")
        .bind(universe_id)
        .bind(handle)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The existing universe a reference names, by its UUID or by its handle.
pub async fn resolve_universe_reference(
    pool: &PgPool,
    reference: &str,
) -> Result<Option<Uuid>, PgStoreError> {
    let query = match Uuid::parse_str(reference) {
        Ok(universe_id) => {
            sqlx::query_scalar("SELECT universe_id FROM universes WHERE universe_id = $1")
                .bind(universe_id)
        }
        Err(_) if is_valid_universe_handle(reference) => {
            sqlx::query_scalar("SELECT universe_id FROM universes WHERE handle = $1")
                .bind(reference)
        }
        Err(_) => return Ok(None),
    };
    Ok(query.fetch_optional(pool).await?)
}

pub async fn read_universe_stats(
    pool: &PgPool,
    universe_id: Uuid,
) -> Result<Option<UniverseStats>, PgStoreError> {
    let query = format!("{UNIVERSE_STATS_SELECT} WHERE u.universe_id = $1");
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(universe_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(universe_stats_from_row).transpose()
}

pub async fn list_universe_stats(pool: &PgPool) -> Result<Vec<UniverseStats>, PgStoreError> {
    let query = format!("{UNIVERSE_STATS_SELECT} ORDER BY u.created_at_ms, u.universe_id");
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .fetch_all(pool)
        .await?;
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

/// Delete the universe row; every universe-scoped table cascades from it,
/// including its keys. `false` when the universe did not exist.
///
/// Deferrable foreign keys between universe-scoped tables (environments to
/// registration keys, environment credentials to auth objects) are checked
/// at commit, so the cascade order between their tables does not matter.
pub async fn delete_universe(pool: &PgPool, universe_id: Uuid) -> Result<bool, PgStoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM universes WHERE universe_id = $1")
        .bind(universe_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

fn universe_stats_from_row(row: &sqlx::postgres::PgRow) -> Result<UniverseStats, PgStoreError> {
    Ok(UniverseStats {
        configuration: api::UniverseConfiguration {
            display_name: row.try_get("display_name")?,
            ..serde_json::from_value(row.try_get("platform_access")?)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?
        },
        universe_id: row.try_get("universe_id")?,
        handle: row.try_get("handle")?,
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

/// Atomically patch declared Platform configuration without overwriting omitted fields.
pub async fn put_universe_configuration(
    pool: &PgPool,
    id: Uuid,
    config: &api::UniverseConfiguration,
) -> Result<bool, PgStoreError> {
    let mut access = serde_json::Map::new();
    if let Some(members) = &config.members {
        access.insert("members".into(), serde_json::json!(members));
    }
    if let Some(groups) = &config.directory_group_mappings {
        access.insert("directoryGroupMappings".into(), serde_json::json!(groups));
    }
    let result = sqlx::query("UPDATE universes SET display_name = COALESCE($2, display_name), platform_access = platform_access || $3 WHERE universe_id = $1")
        .bind(id).bind(&config.display_name).bind(serde_json::Value::Object(access)).execute(pool).await?;
    Ok(result.rows_affected() != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_lowercase_dns_labels() {
        for handle in ["research", "a", "0", "team-2", "2026-research", "a--b"] {
            assert!(is_valid_universe_handle(handle), "{handle}");
        }
        for handle in [
            "",
            "-research",
            "research-",
            "Research",
            "my.universe",
            "my_universe",
            "my:universe",
            "with space",
            "a/b",
            "ümlaut",
        ] {
            assert!(!is_valid_universe_handle(handle), "{handle}");
        }
        assert!(is_valid_universe_handle(
            &"a".repeat(UNIVERSE_HANDLE_MAX_LEN)
        ));
        assert!(!is_valid_universe_handle(
            &"a".repeat(UNIVERSE_HANDLE_MAX_LEN + 1)
        ));
    }

    #[test]
    fn a_handle_never_reads_as_a_uuid() {
        let id = Uuid::from_u128(0x0190_7f3e_8a4c_7b21_9f2c_41d0_7ab3_55e1);
        assert!(!is_valid_universe_handle(&id.hyphenated().to_string()));
        assert!(!is_valid_universe_handle(&id.simple().to_string()));
        // One character short of the bare form is an ordinary name again.
        assert!(is_valid_universe_handle(&id.simple().to_string()[1..]));
    }

    #[test]
    fn the_default_handle_is_the_uuid_tail_and_valid() {
        let id = Uuid::from_u128(0x0190_7f3e_8a4c_7b21_9f2c_41d0_7ab3_55e1);
        let handle = default_universe_handle(id);
        assert_eq!(handle, "u-41d07ab355e1");
        assert!(is_valid_universe_handle(&handle));
        assert_eq!(default_universe_handle(id), handle);
    }
}
