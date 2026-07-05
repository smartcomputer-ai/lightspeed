//! Deployment-scoped API key store (P90 Phase 2).
//!
//! API keys resolve callers *to* a universe, so this store deliberately does
//! not hang off a universe-bound [`crate::PgStore`]: it wraps the shared
//! deployment pool directly, like [`crate::find_auth_flow_universe`].

use auth::{ApiKeyError, ApiKeyRecord, ApiKeyStore, CreateApiKey, PrincipalRef};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::auth::{principal_kind_from_str, principal_kind_to_str};

#[derive(Clone)]
pub struct PgApiKeyStore {
    pool: PgPool,
}

impl PgApiKeyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ApiKeyStore for PgApiKeyStore {
    async fn create_api_key(&self, create: CreateApiKey) -> Result<(), ApiKeyError> {
        let record = create.record;
        let result = sqlx::query(
            r#"
            INSERT INTO api_keys (
                key_hash, key_prefix, universe_id,
                principal_kind, principal_id, display_name,
                created_at_ms, revoked_at_ms, last_used_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, NULL)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&create.key_hash)
        .bind(&record.key_prefix)
        .bind(record.universe_id)
        .bind(principal_kind_to_str(record.principal.kind))
        .bind(record.principal.id.as_deref())
        .bind(record.display_name.as_deref())
        .bind(ms_to_i64(record.created_at_ms)?)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if result.rows_affected() == 0 {
            return Err(ApiKeyError::AlreadyExists {
                key_prefix: record.key_prefix,
            });
        }
        Ok(())
    }

    async fn resolve_api_key(
        &self,
        key_hash: &str,
        observed_at_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        let row = sqlx::query(
            r#"
            UPDATE api_keys
            SET last_used_at_ms = $2
            WHERE key_hash = $1 AND revoked_at_ms IS NULL
            RETURNING key_prefix, universe_id, principal_kind, principal_id,
                      display_name, created_at_ms, revoked_at_ms, last_used_at_ms
            "#,
        )
        .bind(key_hash)
        .bind(ms_to_i64(observed_at_ms)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.map(api_key_record_from_row).transpose()
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>, ApiKeyError> {
        let rows = sqlx::query(
            r#"
            SELECT key_prefix, universe_id, principal_kind, principal_id,
                   display_name, created_at_ms, revoked_at_ms, last_used_at_ms
            FROM api_keys
            ORDER BY created_at_ms, key_prefix
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.into_iter().map(api_key_record_from_row).collect()
    }

    async fn revoke_api_key(
        &self,
        key_prefix: &str,
        revoked_at_ms: u64,
    ) -> Result<bool, ApiKeyError> {
        let result = sqlx::query(
            r#"
            UPDATE api_keys
            SET revoked_at_ms = COALESCE(revoked_at_ms, $2)
            WHERE key_prefix = $1
            "#,
        )
        .bind(key_prefix)
        .bind(ms_to_i64(revoked_at_ms)?)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(result.rows_affected() > 0)
    }
}

fn api_key_record_from_row(row: PgRow) -> Result<ApiKeyRecord, ApiKeyError> {
    let principal_kind: String = row.try_get("principal_kind").map_err(map_sqlx_error)?;
    let principal_kind =
        principal_kind_from_str(&principal_kind).map_err(|error| ApiKeyError::Store {
            message: error.to_string(),
        })?;
    let universe_id: Uuid = row.try_get("universe_id").map_err(map_sqlx_error)?;
    Ok(ApiKeyRecord {
        key_prefix: row.try_get("key_prefix").map_err(map_sqlx_error)?,
        universe_id,
        principal: PrincipalRef {
            kind: principal_kind,
            id: row.try_get("principal_id").map_err(map_sqlx_error)?,
        },
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
