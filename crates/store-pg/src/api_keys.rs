//! Deployment-scoped API key store.
//!
//! The store uses the deployment pool because authentication precedes universe
//! resolution. Issuance serializes with identity changes and audits the issuer.

use auth::{ApiKeyError, ApiKeyRecord, ApiKeyStore, CreateApiKey};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::access::{audit, effective};
use access::AccessScope;

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
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT revision FROM access_policy WHERE singleton FOR UPDATE")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let actor = effective(&mut tx, record.created_by, record.scope)
            .await
            .map_err(map_access_error)?;
        let deployment = effective(&mut tx, record.created_by, AccessScope::Deployment)
            .await
            .map_err(map_access_error)?;
        let target = effective(&mut tx, record.principal_id, record.scope)
            .await
            .map_err(map_access_error)?;
        if !access::may_issue_key(
            create.authority_scope,
            &actor,
            &deployment,
            &target.principal,
        ) {
            return Err(ApiKeyError::Denied);
        }
        let result = sqlx::query(
            r#"
            INSERT INTO api_keys (
                key_hash, key_prefix, universe_id,
                principal_id, created_by, display_name,
                created_at_ms, revoked_at_ms, last_used_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, NULL)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(&create.key_hash)
        .bind(&record.key_prefix)
        .bind(scope_id(record.scope))
        .bind(record.principal_id)
        .bind(record.created_by)
        .bind(record.display_name.as_deref())
        .bind(ms_to_i64(record.created_at_ms)?)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if result.rows_affected() == 0 {
            return Err(ApiKeyError::AlreadyExists {
                key_prefix: record.key_prefix,
            });
        }
        audit(
            &mut tx,
            Some(record.created_by),
            serde_json::json!({
                "operation": "key_created", "keyPrefix": record.key_prefix,
                "principalId": record.principal_id, "scope": record.scope
            }),
            ms_to_i64(record.created_at_ms)?,
        )
        .await
        .map_err(map_access_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
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
              AND EXISTS (SELECT 1 FROM access_principals p
                          WHERE p.principal_id = api_keys.principal_id AND p.status = 'active')
            RETURNING key_prefix, universe_id, principal_id, created_by,
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
            SELECT key_prefix, universe_id, principal_id, created_by,
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

    async fn list_api_keys_for_universe(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<ApiKeyRecord>, ApiKeyError> {
        let rows = sqlx::query(
            r#"
            SELECT key_prefix, universe_id, principal_id, created_by,
                   display_name, created_at_ms, revoked_at_ms, last_used_at_ms
            FROM api_keys
            WHERE universe_id = $1
            ORDER BY created_at_ms, key_prefix
            "#,
        )
        .bind(universe_id)
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

    async fn revoke_api_key_for_universe(
        &self,
        universe_id: Uuid,
        key_prefix: &str,
        revoked_at_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        let row = sqlx::query(
            r#"
            UPDATE api_keys
            SET revoked_at_ms = COALESCE(revoked_at_ms, $3)
            WHERE universe_id = $1 AND key_prefix = $2
            RETURNING key_prefix, universe_id, principal_id, created_by,
                      display_name, created_at_ms, revoked_at_ms, last_used_at_ms
            "#,
        )
        .bind(universe_id)
        .bind(key_prefix)
        .bind(ms_to_i64(revoked_at_ms)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.map(api_key_record_from_row).transpose()
    }
}

fn api_key_record_from_row(row: PgRow) -> Result<ApiKeyRecord, ApiKeyError> {
    let universe_id: Option<Uuid> = row.try_get("universe_id").map_err(map_sqlx_error)?;
    Ok(ApiKeyRecord {
        key_prefix: row.try_get("key_prefix").map_err(map_sqlx_error)?,
        scope: universe_id.map_or(AccessScope::Deployment, |universe_id| {
            AccessScope::Universe { universe_id }
        }),
        principal_id: row.try_get("principal_id").map_err(map_sqlx_error)?,
        created_by: row.try_get("created_by").map_err(map_sqlx_error)?,
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

fn scope_id(scope: AccessScope) -> Option<Uuid> {
    match scope {
        AccessScope::Deployment => None,
        AccessScope::Universe { universe_id } => Some(universe_id),
    }
}
fn map_access_error(error: access::AccessError) -> ApiKeyError {
    match error {
        access::AccessError::Denied | access::AccessError::NotFound => ApiKeyError::Denied,
        _ => ApiKeyError::Store {
            message: error.to_string(),
        },
    }
}

impl PgApiKeyStore {
    /// Members see their own keys. Scope administrators see keys in their
    /// scope, without acquiring authority to mint integration identities.
    pub async fn list_managed_keys(
        &self,
        actor: Uuid,
        authority_scope: AccessScope,
        scope: AccessScope,
    ) -> Result<Vec<ApiKeyRecord>, ApiKeyError> {
        if authority_scope != AccessScope::Deployment && authority_scope != scope {
            return Err(ApiKeyError::Denied);
        }
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let rights = effective(&mut tx, actor, scope)
            .await
            .map_err(map_access_error)?;
        let deployment = effective(&mut tx, actor, AccessScope::Deployment)
            .await
            .map_err(map_access_error)?;
        let admin = rights.has_role(access::Role::Admin)
            || (authority_scope == AccessScope::Deployment
                && deployment.has_role(access::Role::DeploymentAdmin));
        if !rights.active() || (!admin && rights.roles.is_empty()) {
            return Err(ApiKeyError::Denied);
        }
        let rows = sqlx::query("SELECT * FROM api_keys WHERE universe_id IS NOT DISTINCT FROM $1 AND ($2 OR principal_id = $3) ORDER BY created_at_ms, key_prefix")
            .bind(scope_id(scope)).bind(admin).bind(actor).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        rows.into_iter().map(api_key_record_from_row).collect()
    }

    pub async fn revoke_managed_key(
        &self,
        actor: Uuid,
        authority_scope: AccessScope,
        scope: AccessScope,
        prefix: &str,
        now_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        if authority_scope != AccessScope::Deployment && authority_scope != scope {
            return Err(ApiKeyError::Denied);
        }
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT revision FROM access_policy WHERE singleton FOR UPDATE")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let rights = effective(&mut tx, actor, scope)
            .await
            .map_err(map_access_error)?;
        let deployment = effective(&mut tx, actor, AccessScope::Deployment)
            .await
            .map_err(map_access_error)?;
        let admin = rights.has_role(access::Role::Admin)
            || (authority_scope == AccessScope::Deployment
                && deployment.has_role(access::Role::DeploymentAdmin));
        if !rights.active() || (!admin && rights.roles.is_empty()) {
            return Err(ApiKeyError::Denied);
        }
        let mut record = sqlx::query("SELECT * FROM api_keys WHERE universe_id IS NOT DISTINCT FROM $1 AND key_prefix = $2 AND ($3 OR principal_id = $4) FOR UPDATE")
            .bind(scope_id(scope)).bind(prefix).bind(admin).bind(actor)
            .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?
            .map(api_key_record_from_row).transpose()?;
        if let Some(record) = record.as_mut()
            && record.revoked_at_ms.is_none()
        {
            sqlx::query("UPDATE api_keys SET revoked_at_ms = $2 WHERE key_prefix = $1")
                .bind(prefix)
                .bind(ms_to_i64(now_ms)?)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            record.revoked_at_ms = Some(now_ms);
            audit(
                &mut tx,
                Some(actor),
                serde_json::json!({"operation":"key_revoked", "keyPrefix":prefix, "scope":scope}),
                ms_to_i64(now_ms)?,
            )
            .await
            .map_err(map_access_error)?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(record)
    }
}
