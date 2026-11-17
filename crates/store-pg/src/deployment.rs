//! Deployment-level universe administration.
//!
//! Everything here runs against the shared pool, above the universe-bound
//! [`PgStore`](crate::PgStore) boundary — the deployment API addresses the set
//! of universes, so no per-universe store applies. Stats are cheap aggregates
//! computed at read time, approximate under concurrent writes by design.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::PgStoreError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniverseStats {
    pub configuration: api::UniverseConfiguration,
    pub universe_id: Uuid,
    pub handle: String,
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    /// `max(sessions.updated_at_ms)`; `None` when the universe has no
    /// sessions.
    pub last_activity_at_ms: Option<i64>,
    pub sessions: u64,
    pub workspaces: u64,
    pub profiles: u64,
    pub blob_bytes: u64,
    pub deletion: Option<api::UniverseDeletion>,
}

const UNIVERSE_STATS_SELECT: &str = r#"
    SELECT
        u.universe_id,
        u.handle,
        u.display_name,
        u.metadata_json,
        u.protected,
        u.expires_at_ms,
        u.created_at_ms,
        u.deletion_json,
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
    create_universe_with_configuration(pool, universe_id, handle, &BTreeMap::new(), false, None)
        .await
}

/// Creation settings are atomic and never overwrite an existing universe.
pub async fn create_universe_with_configuration(
    pool: &PgPool,
    universe_id: Uuid,
    handle: Option<&str>,
    metadata: &BTreeMap<String, String>,
    protected: bool,
    expires_at_ms: Option<i64>,
) -> Result<bool, PgStoreError> {
    validate_universe_metadata(metadata)?;
    validate_protection_expiry(protected, expires_at_ms)?;
    validate_expiry(expires_at_ms)?;
    let mut tx = pool.begin().await?;
    // Serialize creation with finalization, including the gap after row deletion.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(universe_id.to_string())
        .execute(&mut *tx)
        .await?;
    let unavailable: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM universes_deleted WHERE universe_id = $1) OR EXISTS(SELECT 1 FROM universes WHERE universe_id = $1 AND deleting_at_ms IS NOT NULL)"
    ).bind(universe_id).fetch_one(&mut *tx).await?;
    if unavailable {
        return Err(PgStoreError::UniverseDeleted { universe_id });
    }
    validate_expiry(expires_at_ms)?;
    let result = sqlx::query(
        "INSERT INTO universes (universe_id, handle, metadata_json, protected, expires_at_ms) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (universe_id) DO NOTHING"
    ).bind(universe_id)
        .bind(handle.map_or_else(|| default_universe_handle(universe_id), str::to_owned))
        .bind(serde_json::json!(metadata)).bind(protected).bind(expires_at_ms)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// Rename a universe without changing its identity; the previous handle is
/// released. `false` when the universe does not exist.
pub async fn put_universe_handle(
    pool: &PgPool,
    universe_id: Uuid,
    handle: &str,
) -> Result<bool, PgStoreError> {
    let result = sqlx::query(
        "UPDATE universes SET handle = $2 WHERE universe_id = $1 AND deleting_at_ms IS NULL",
    )
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
        Ok(universe_id) => sqlx::query_scalar(
            "SELECT universe_id FROM universes WHERE universe_id = $1 AND deleting_at_ms IS NULL",
        )
        .bind(universe_id),
        Err(_) if is_valid_universe_handle(reference) => sqlx::query_scalar(
            "SELECT universe_id FROM universes WHERE handle = $1 AND deleting_at_ms IS NULL",
        )
        .bind(reference),
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
    list_universe_stats_matching(pool, &BTreeMap::new()).await
}

pub async fn list_universe_stats_matching(
    pool: &PgPool,
    metadata: &BTreeMap<String, String>,
) -> Result<Vec<UniverseStats>, PgStoreError> {
    validate_universe_metadata(metadata)?;
    let query = format!(
        "{UNIVERSE_STATS_SELECT} WHERE u.metadata_json @> $1 ORDER BY u.created_at_ms, u.universe_id"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(serde_json::json!(metadata))
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
    let protected: Option<bool> =
        sqlx::query_scalar("SELECT protected FROM universes WHERE universe_id = $1 FOR UPDATE")
            .bind(universe_id)
            .fetch_optional(&mut *tx)
            .await?;
    if protected == Some(true) {
        return Err(PgStoreError::UniverseProtected { universe_id });
    }
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
            metadata: Some(
                serde_json::from_value(row.try_get("metadata_json")?)
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            ),
            protected: Some(row.try_get("protected")?),
        },
        universe_id: row.try_get("universe_id")?,
        handle: row.try_get("handle")?,
        expires_at_ms: row.try_get("expires_at_ms")?,
        created_at_ms: row.try_get("created_at_ms")?,
        last_activity_at_ms: row.try_get("last_activity_at_ms")?,
        sessions: count_to_u64(row.try_get("sessions")?),
        workspaces: count_to_u64(row.try_get("workspaces")?),
        profiles: count_to_u64(row.try_get("profiles")?),
        blob_bytes: count_to_u64(row.try_get("blob_bytes")?),
        deletion: row
            .try_get::<Option<serde_json::Value>, _>("deletion_json")?
            .map(|value| {
                serde_json::from_value::<UniverseDeletionRecord>(value)
                    .map(|record| record.deletion)
            })
            .transpose()
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
    })
}

/// Counts and sums are non-negative by construction; clamp rather than fail.
fn count_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn validate_universe_metadata(metadata: &BTreeMap<String, String>) -> Result<(), PgStoreError> {
    environment_protocol::metadata::validate_caller_metadata(metadata).map_err(|error| {
        PgStoreError::InvalidUniverseConfiguration {
            message: error.to_string(),
        }
    })
}
fn validate_protection_expiry(protected: bool, expiry: Option<i64>) -> Result<(), PgStoreError> {
    if protected && expiry.is_some() {
        return Err(PgStoreError::UniverseProtectionExpiryConflict);
    }
    Ok(())
}
fn validate_expiry(expiry: Option<i64>) -> Result<(), PgStoreError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| PgStoreError::Store {
            message: error.to_string(),
        })?
        .as_millis();
    if expiry.is_some_and(|time| time <= 0 || time as u128 <= now) {
        return Err(PgStoreError::InvalidUniverseConfiguration {
            message: "expiresAtMs must be in the future".into(),
        });
    }
    Ok(())
}

/// Patch resource configuration under the same row lock used by deletion and expiry.
pub async fn put_universe_configuration(
    pool: &PgPool,
    id: Uuid,
    config: &api::UniverseConfiguration,
) -> Result<bool, PgStoreError> {
    config
        .validate()
        .map_err(|message| PgStoreError::InvalidUniverseConfiguration {
            message: message.into(),
        })?;
    if let Some(metadata) = &config.metadata {
        validate_universe_metadata(metadata)?;
    }
    let mut tx = pool.begin().await?;
    let current: Option<(bool, Option<i64>)> = sqlx::query_as("SELECT protected, expires_at_ms FROM universes WHERE universe_id = $1 AND deleting_at_ms IS NULL FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let Some((protected, expiry)) = current else {
        return Ok(false);
    };
    validate_protection_expiry(config.protected.unwrap_or(protected), expiry)?;
    sqlx::query("UPDATE universes SET display_name = COALESCE($2, display_name), metadata_json = COALESCE($3, metadata_json), protected = COALESCE($4, protected) WHERE universe_id = $1")
        .bind(id).bind(&config.display_name).bind(config.metadata.as_ref().map(serde_json::to_value).transpose().map_err(|error| PgStoreError::Store {message:error.to_string()})?).bind(config.protected).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn put_universe_expiry(
    pool: &PgPool,
    id: Uuid,
    expiry: Option<i64>,
) -> Result<bool, PgStoreError> {
    validate_expiry(expiry)?;
    let mut tx = pool.begin().await?;
    let protected: Option<bool> = sqlx::query_scalar("SELECT protected FROM universes WHERE universe_id = $1 AND deleting_at_ms IS NULL FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let Some(protected) = protected else {
        return Ok(false);
    };
    validate_protection_expiry(protected, expiry)?;
    validate_expiry(expiry)?;
    sqlx::query("UPDATE universes SET expires_at_ms = $2 WHERE universe_id = $1")
        .bind(id)
        .bind(expiry)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn universe_metadata_reuses_bounded_caller_metadata() {
        assert!(
            validate_universe_metadata(&BTreeMap::from([("purpose".into(), "ci".into())])).is_ok()
        );
        for metadata in [
            BTreeMap::from([("lightspeed.hidden".into(), "x".into())]),
            BTreeMap::from([("x".repeat(65), "x".into())]),
            BTreeMap::from([("x".into(), "x".repeat(257))]),
            (0..33)
                .map(|index| (index.to_string(), "x".into()))
                .collect(),
        ] {
            assert!(matches!(
                validate_universe_metadata(&metadata),
                Err(PgStoreError::InvalidUniverseConfiguration { .. })
            ));
        }
    }
    #[test]
    fn protection_and_expiry_are_exclusive() {
        assert!(validate_protection_expiry(true, None).is_ok());
        assert!(validate_protection_expiry(false, Some(i64::MAX)).is_ok());
        assert!(matches!(
            validate_protection_expiry(true, Some(i64::MAX)),
            Err(PgStoreError::UniverseProtectionExpiryConflict)
        ));
        assert!(matches!(
            validate_expiry(Some(0)),
            Err(PgStoreError::InvalidUniverseConfiguration { .. })
        ));
        assert!(validate_expiry(None).is_ok());
    }

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

/// Durable teardown cursor. `progress` is internal retry bookkeeping, separate
/// from the permanent, public inventory summary.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UniverseDeletionRecord {
    pub universe_id: Uuid,
    pub deletion: api::UniverseDeletion,
    pub summary: serde_json::Value,
    #[serde(default)]
    pub progress: serde_json::Value,
}

/// Set the admission fence and capture the inventory in the same statement.
/// The immutable snapshot is retained when a caller repeats the request.
pub async fn begin_universe_deletion(
    pool: &PgPool,
    universe_id: Uuid,
    requested_by: &api::Attribution,
    now: i64,
) -> Result<Option<api::UniverseDeletionState>, PgStoreError> {
    let mut tx = pool.begin().await?;
    let state =
        begin_universe_deletion_locked(&mut tx, universe_id, requested_by, now, false).await?;
    tx.commit().await?;
    Ok(state)
}

async fn begin_universe_deletion_locked(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    universe_id: Uuid,
    requested_by: &api::Attribution,
    now: i64,
    expired_only: bool,
) -> Result<Option<api::UniverseDeletionState>, PgStoreError> {
    let row: Option<(bool, Option<i64>, Option<i64>)> = sqlx::query_as("SELECT protected, expires_at_ms, deleting_at_ms FROM universes WHERE universe_id = $1 FOR UPDATE")
        .bind(universe_id).fetch_optional(&mut **tx).await?;
    if let Some((protected, expiry, deleting)) = row {
        if protected {
            if expired_only {
                return Ok(None);
            }
            return Err(PgStoreError::UniverseProtected { universe_id });
        }
        if expired_only && (deleting.is_some() || expiry.is_none_or(|time| time > now)) {
            return Ok(None);
        }
    } else if expired_only {
        return Ok(None);
    }
    let result = sqlx::query(r#"
        UPDATE universes u SET deleting_at_ms = $2,
        deletion_json = jsonb_build_object(
            'universeId', u.universe_id,
            'deletion', jsonb_build_object('startedAtMs', $2::bigint, 'requestedBy', $3::jsonb, 'step', 'intake'),
            'progress', '{}'::jsonb,
            'summary', jsonb_build_object(
                'contained', jsonb_build_object(
                    'sessions', (SELECT count(*) FROM sessions WHERE universe_id = u.universe_id),
                    'bots', (SELECT count(*) FROM bots WHERE universe_id = u.universe_id),
                    'workspaces', (SELECT count(*) FROM vfs_workspaces WHERE universe_id = u.universe_id),
                    'profiles', (SELECT count(*) FROM agent_profiles WHERE universe_id = u.universe_id),
                    'environments', jsonb_build_object(
                        'provisioned', (SELECT count(*) FROM environments WHERE universe_id = u.universe_id AND source_kind = 'provisioned'),
                        'registered', (SELECT count(*) FROM environments WHERE universe_id = u.universe_id AND source_kind = 'registered'),
                        'external', (SELECT count(*) FROM environments WHERE universe_id = u.universe_id AND source_kind = 'external')),
                    'mcpServers', (SELECT count(*) FROM mcp_servers WHERE universe_id = u.universe_id),
                    'blobBytes', COALESCE((SELECT sum(byte_len) FROM cas_blobs WHERE universe_id = u.universe_id), 0),
                    'lastActivityAtMs', (SELECT max(updated_at_ms) FROM sessions WHERE universe_id = u.universe_id)),
                'removed', jsonb_build_object('workflowsTerminated', 0, 'schedulesDeleted', 0,
                    'objectsDeleted', 0, 'environmentsClosed', 0, 'environmentsUnconfirmed', '[]'::jsonb)))
        WHERE u.universe_id = $1 AND u.deleting_at_ms IS NULL
    "#).bind(universe_id).bind(now).bind(serde_json::json!(requested_by)).execute(&mut **tx).await?;
    if result.rows_affected() > 0 {
        return Ok(Some(api::UniverseDeletionState::Deleting));
    }
    // One snapshot avoids returning unknown during the final row/tombstone swap.
    let state: Option<String> = sqlx::query_scalar(
        "SELECT 'deleting' FROM universes WHERE universe_id = $1 AND deleting_at_ms IS NOT NULL UNION ALL SELECT 'deleted' FROM universes_deleted WHERE universe_id = $1 LIMIT 1"
    ).bind(universe_id).fetch_optional(&mut **tx).await?;
    Ok(state.map(|state| {
        if state == "deleted" {
            api::UniverseDeletionState::Deleted
        } else {
            api::UniverseDeletionState::Deleting
        }
    }))
}

/// Start due deletions, rechecking expiry while holding each universe row lock.
/// A concurrent extension or protection update wins or loses atomically with admission.
pub async fn expire_universes(pool: &PgPool, now: i64) -> Result<Vec<Uuid>, PgStoreError> {
    let candidates: Vec<Uuid> = sqlx::query_scalar("SELECT universe_id FROM universes WHERE expires_at_ms <= $1 AND deleting_at_ms IS NULL AND NOT protected ORDER BY expires_at_ms, universe_id LIMIT 100")
        .bind(now).fetch_all(pool).await?;
    let attribution = api::Attribution::Internal {
        component: "universe-expiry".into(),
        cause: "expired".into(),
    };
    let mut admitted = Vec::new();
    for id in candidates {
        let mut tx = pool.begin().await?;
        if begin_universe_deletion_locked(&mut tx, id, &attribution, now, true)
            .await?
            .is_some()
        {
            admitted.push(id);
        }
        tx.commit().await?;
    }
    Ok(admitted)
}

pub async fn list_deleting_universes(
    pool: &PgPool,
) -> Result<Vec<UniverseDeletionRecord>, PgStoreError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT deletion_json FROM universes WHERE deleting_at_ms IS NOT NULL ORDER BY deleting_at_ms, universe_id"
    ).fetch_all(pool).await?;
    rows.into_iter()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
        })
        .collect()
}

pub async fn update_universe_deletion(
    pool: &PgPool,
    universe_id: Uuid,
    record: &UniverseDeletionRecord,
) -> Result<(), PgStoreError> {
    sqlx::query("UPDATE universes SET deletion_json = $2 WHERE universe_id = $1 AND deleting_at_ms IS NOT NULL")
        .bind(universe_id).bind(serde_json::json!(record)).execute(pool).await?;
    Ok(())
}

/// Atomically retain the permanent record and cascade all universe-owned rows.
/// Only the storage step may finalize; callers must first empty the object prefix.
pub async fn finish_universe_deletion(
    pool: &PgPool,
    universe_id: Uuid,
    now: i64,
) -> Result<bool, PgStoreError> {
    let mut tx = pool.begin().await?;
    let removed = finish_universe_deletion_in_transaction(&mut tx, universe_id, now).await?;
    tx.commit().await?;
    Ok(removed)
}

/// Finalize while holding the caller's object-cleanup lock transaction.
pub async fn finish_universe_deletion_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    universe_id: Uuid,
    now: i64,
) -> Result<bool, PgStoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(universe_id.to_string())
        .execute(&mut **tx)
        .await?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut **tx)
        .await?;
    let result = sqlx::query(r#"
        INSERT INTO universes_deleted
            (universe_id, handle, display_name, metadata_json, created_at_ms, deleting_at_ms, deleted_at_ms, requested_by, summary)
        SELECT universe_id, handle, display_name, metadata_json, created_at_ms, deleting_at_ms, $2,
            deletion_json->'deletion'->'requestedBy', deletion_json->'summary'
        FROM universes WHERE universe_id = $1 AND deleting_at_ms IS NOT NULL
            AND deletion_json->'deletion'->>'step' = 'storage'
        ON CONFLICT (universe_id) DO NOTHING
    "#).bind(universe_id).bind(now).execute(&mut **tx).await?;
    if result.rows_affected() > 0 {
        sqlx::query("DELETE FROM universes WHERE universe_id = $1")
            .bind(universe_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(result.rows_affected() > 0)
}

fn deleted_universe_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<api::DeletedUniverseView, PgStoreError> {
    Ok(api::DeletedUniverseView {
        metadata: serde_json::from_value(row.try_get("metadata_json")?)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
        universe_id: row.try_get::<Uuid, _>("universe_id")?.to_string(),
        handle: row.try_get("handle")?,
        display_name: row.try_get("display_name")?,
        created_at_ms: count_to_u64(row.try_get("created_at_ms")?),
        deleting_at_ms: count_to_u64(row.try_get("deleting_at_ms")?),
        deleted_at_ms: count_to_u64(row.try_get("deleted_at_ms")?),
        requested_by: serde_json::from_value(row.try_get("requested_by")?)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
        summary: row.try_get("summary")?,
    })
}

pub async fn read_deleted_universe(
    pool: &PgPool,
    universe_id: Uuid,
) -> Result<Option<api::DeletedUniverseView>, PgStoreError> {
    let row = sqlx::query("SELECT * FROM universes_deleted WHERE universe_id = $1")
        .bind(universe_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(deleted_universe_from_row).transpose()
}

pub async fn list_deleted_universes(
    pool: &PgPool,
    after: Option<&str>,
) -> Result<api::DeploymentDeletedUniversesListResponse, PgStoreError> {
    let cursor = after
        .map(|value| {
            let (time, id) = value.split_once(':').ok_or_else(|| PgStoreError::Store {
                message: "invalid deleted universe cursor".into(),
            })?;
            Ok::<_, PgStoreError>((
                time.parse::<i64>().map_err(|_| PgStoreError::Store {
                    message: "invalid deleted universe cursor timestamp".into(),
                })?,
                Uuid::parse_str(id).map_err(|_| PgStoreError::Store {
                    message: "invalid deleted universe cursor UUID".into(),
                })?,
            ))
        })
        .transpose()?;
    let rows = sqlx::query("SELECT * FROM universes_deleted WHERE ($1::bigint IS NULL OR (deleted_at_ms, universe_id) < ($1, $2::uuid)) ORDER BY deleted_at_ms DESC, universe_id DESC LIMIT 101")
        .bind(cursor.map(|(time, _)| time)).bind(cursor.map(|(_, id)| id)).fetch_all(pool).await?;
    let has_more = rows.len() > 100;
    let universes = rows
        .iter()
        .take(100)
        .map(deleted_universe_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = if has_more {
        universes
            .last()
            .map(|u| format!("{}:{}", u.deleted_at_ms, u.universe_id))
    } else {
        None
    };
    Ok(api::DeploymentDeletedUniversesListResponse {
        universes,
        next_after,
    })
}
