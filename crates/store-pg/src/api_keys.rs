//! Deployment-scoped API key store.
//!
//! The store uses the deployment pool because authentication precedes
//! universe resolution. A key's authority is its row: scope, groups and the
//! actor flag. Keys change only by revocation.

use std::collections::BTreeSet;

use api::{AccessScope, MethodGroup};
use auth::{ApiKeyError, ApiKeyRecord};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

const KEY_COLUMNS: &str = "key_prefix, universe_id, groups, assert_actor, display_name, created_by, created_at_ms, revoked_at_ms, last_used_at_ms";

#[derive(Clone)]
pub struct PgApiKeyStore {
    pool: PgPool,
}

impl PgApiKeyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persist a minted key. A prefix already in use is `AlreadyExists`, so
    /// the caller mints again.
    pub async fn create_api_key(
        &self,
        key_hash: &str,
        record: &ApiKeyRecord,
    ) -> Result<(), ApiKeyError> {
        let groups: Vec<&str> = record.groups.iter().map(|group| group.as_str()).collect();
        let inserted = sqlx::query(
            "INSERT INTO api_keys (key_hash, key_prefix, universe_id, groups, assert_actor, display_name, created_by, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
        )
        .bind(key_hash)
        .bind(&record.key_prefix)
        .bind(record.scope.universe_id())
        .bind(groups)
        .bind(record.assert_actor)
        .bind(record.display_name.as_deref())
        .bind(json!(record.created_by))
        .bind(ms_to_i64(record.created_at_ms)?)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        .rows_affected();
        if inserted == 0 {
            return Err(ApiKeyError::AlreadyExists {
                key_prefix: record.key_prefix.clone(),
            });
        }
        Ok(())
    }

    /// Resolve an unrevoked key by secret hash; `None` for unknown and
    /// revoked keys alike.
    pub async fn resolve_api_key(
        &self,
        key_hash: &str,
        observed_at_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        let Some(row) = sqlx::query(&format!(
            "SELECT {KEY_COLUMNS} FROM api_keys WHERE key_hash = $1 AND revoked_at_ms IS NULL"
        ))
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        else {
            return Ok(None);
        };
        let record = record_from_row(&row)?;
        // Usage is coarse on purpose: authentication stays a read.
        if record.last_used_at_ms.is_none_or(|last| {
            last.saturating_add(auth::API_KEY_LAST_USED_RESOLUTION_MS) <= observed_at_ms
        }) {
            sqlx::query(
                "UPDATE api_keys SET last_used_at_ms = $2 WHERE key_hash = $1 AND \
                 (last_used_at_ms IS NULL OR last_used_at_ms < $2)",
            )
            .bind(key_hash)
            .bind(ms_to_i64(observed_at_ms)?)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        }
        Ok(Some(record))
    }

    /// Every key, or the keys of one scope.
    pub async fn list_api_keys(
        &self,
        scope: Option<AccessScope>,
    ) -> Result<Vec<ApiKeyRecord>, ApiKeyError> {
        let (filtered, universe_id) = match scope {
            None => (false, None),
            Some(scope) => (true, scope.universe_id()),
        };
        sqlx::query(&format!(
            "SELECT {KEY_COLUMNS} FROM api_keys
             WHERE NOT $1 OR universe_id IS NOT DISTINCT FROM $2
             ORDER BY created_at_ms, key_prefix"
        ))
        .bind(filtered)
        .bind(universe_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        .iter()
        .map(record_from_row)
        .collect()
    }

    /// Revoke a key by its display prefix. `None` for an unknown prefix; a
    /// revoked key stays revoked at its first revocation time.
    pub async fn revoke_api_key(
        &self,
        key_prefix: &str,
        now_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        sqlx::query(&format!(
            "UPDATE api_keys SET revoked_at_ms = COALESCE(revoked_at_ms, $2)
             WHERE key_prefix = $1 RETURNING {KEY_COLUMNS}"
        ))
        .bind(key_prefix)
        .bind(ms_to_i64(now_ms)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        .as_ref()
        .map(record_from_row)
        .transpose()
    }
}

fn record_from_row(row: &PgRow) -> Result<ApiKeyRecord, ApiKeyError> {
    let universe_id: Option<Uuid> = row.try_get("universe_id").map_err(map_sqlx_error)?;
    let groups = row
        .try_get::<Vec<String>, _>("groups")
        .map_err(map_sqlx_error)?
        .iter()
        .map(|name| {
            MethodGroup::parse(name).ok_or_else(|| ApiKeyError::Store {
                message: format!("unknown method group in api_keys row: {name}"),
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(ApiKeyRecord {
        key_prefix: row.try_get("key_prefix").map_err(map_sqlx_error)?,
        scope: universe_id.map_or(AccessScope::Deployment, |universe_id| {
            AccessScope::Universe { universe_id }
        }),
        groups,
        assert_actor: row.try_get("assert_actor").map_err(map_sqlx_error)?,
        created_by: serde_json::from_value(row.try_get("created_by").map_err(map_sqlx_error)?)
            .map_err(|error| ApiKeyError::Store {
                message: format!("decode api key created_by: {error}"),
            })?,
        display_name: row.try_get("display_name").map_err(map_sqlx_error)?,
        created_at_ms: i64_to_ms(row.try_get("created_at_ms").map_err(map_sqlx_error)?)?,
        revoked_at_ms: row
            .try_get::<Option<i64>, _>("revoked_at_ms")
            .map_err(map_sqlx_error)?
            .map(i64_to_ms)
            .transpose()?,
        last_used_at_ms: row
            .try_get::<Option<i64>, _>("last_used_at_ms")
            .map_err(map_sqlx_error)?
            .map(i64_to_ms)
            .transpose()?,
    })
}

fn ms_to_i64(value: u64) -> Result<i64, ApiKeyError> {
    i64::try_from(value).map_err(|_| ApiKeyError::Store {
        message: format!("timestamp out of range: {value}"),
    })
}

fn i64_to_ms(value: i64) -> Result<u64, ApiKeyError> {
    u64::try_from(value).map_err(|_| ApiKeyError::Store {
        message: format!("negative timestamp in api_keys row: {value}"),
    })
}

fn map_sqlx_error(error: sqlx::Error) -> ApiKeyError {
    ApiKeyError::Store {
        message: error.to_string(),
    }
}
