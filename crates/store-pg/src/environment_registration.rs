//! Registration keys for outbound `envd` registration, plus the two
//! deployment-wide lookups the environment gateway needs before it knows a
//! universe: a presented secret selects the universe of its key, and a
//! known daemon public key selects the universe of its environment.

use async_trait::async_trait;
use environments::{
    CreateEnvironmentRegistrationKey, EnvironmentRegistrationKeyId,
    EnvironmentRegistrationKeyRecord, EnvironmentRegistrationKeyStore, EnvironmentRegistryError,
    RegistrationKeyUsage, RevokeEnvironmentRegistrationKey,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    PgStore, PgStoreError,
    environment::{identity_mode_from_str, invalid, not_found, sql_error, store_message},
};

const KEY_COLUMNS: &str = r#"
    registration_key_id, display_name, key_prefix, identity_mode,
    max_active_environments, ephemeral_disconnect_grace_ms, expires_at_ms,
    created_at_ms, revoked_at_ms
"#;

#[async_trait]
impl EnvironmentRegistrationKeyStore for PgStore {
    async fn create_registration_key(
        &self,
        request: CreateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        request.record.validate()?;
        self.ensure_universe()
            .await
            .map_err(|error| store_message(format!("ensure universe: {error}")))?;
        let record = request.record;
        let query = format!(
            r#"
            INSERT INTO environment_registration_keys (
                universe_id, registration_key_id, display_name, key_prefix, secret_hash,
                identity_mode, max_active_environments, ephemeral_disconnect_grace_ms,
                expires_at_ms, created_at_ms, revoked_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
            RETURNING {KEY_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.registration_key_id.as_str())
            .bind(&record.display_name)
            .bind(&record.key_prefix)
            .bind(&request.secret_hash)
            .bind(record.identity_mode.as_str())
            .bind(
                record
                    .max_active_environments
                    .map(|value| i32::try_from(value).unwrap_or(i32::MAX)),
            )
            .bind(
                record
                    .ephemeral_disconnect_grace_ms
                    .map(|value| i64::try_from(value).unwrap_or(i64::MAX)),
            )
            .bind(record.expires_at_ms)
            .bind(record.created_at_ms)
            .bind(record.revoked_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| map_key_insert_error(error, &record.registration_key_id))?;
        key_from_row(&row)
    }

    async fn read_registration_key(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {KEY_COLUMNS} FROM environment_registration_keys WHERE universe_id = $1 AND registration_key_id = $2"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(registration_key_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read registration key", error))?
            .ok_or_else(|| not_found("environment_registration_key", registration_key_id))?;
        key_from_row(&row)
    }

    async fn list_registration_keys(
        &self,
    ) -> Result<Vec<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {KEY_COLUMNS} FROM environment_registration_keys WHERE universe_id = $1 ORDER BY created_at_ms, registration_key_id"
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list registration keys", error))?;
        rows.iter().map(key_from_row).collect()
    }

    async fn revoke_registration_key(
        &self,
        request: RevokeEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        if request.revoked_at_ms < 0 {
            return invalid("revoked_at_ms must be nonnegative");
        }
        let query = format!(
            r#"
            UPDATE environment_registration_keys
            SET revoked_at_ms = COALESCE(revoked_at_ms, GREATEST($3, created_at_ms))
            WHERE universe_id = $1 AND registration_key_id = $2
            RETURNING {KEY_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.registration_key_id.as_str())
            .bind(request.revoked_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("revoke registration key", error))?
            .ok_or_else(|| {
                not_found("environment_registration_key", &request.registration_key_id)
            })?;
        key_from_row(&row)
    }

    async fn resolve_registration_key(
        &self,
        secret_hash: &str,
    ) -> Result<Option<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {KEY_COLUMNS} FROM environment_registration_keys WHERE universe_id = $1 AND secret_hash = $2"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(secret_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("resolve registration key", error))?;
        row.as_ref().map(key_from_row).transpose()
    }

    async fn registration_key_usage(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<RegistrationKeyUsage, EnvironmentRegistryError> {
        self.read_registration_key(registration_key_id).await?;
        let row = sqlx::query(
            r#"
            SELECT count(*) AS registered,
                   count(*) FILTER (WHERE status <> 'closed') AS active,
                   max(created_at_ms) AS last_registered_at_ms
            FROM environments
            WHERE universe_id = $1 AND registration_key_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(registration_key_id.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|error| sql_error("count registration key usage", error))?;
        let registered: i64 = row
            .try_get("registered")
            .map_err(|error| sql_error("decode registered count", error))?;
        let active: i64 = row
            .try_get("active")
            .map_err(|error| sql_error("decode active count", error))?;
        Ok(RegistrationKeyUsage {
            registered: registered.max(0) as u64,
            active: active.max(0) as u64,
            last_registered_at_ms: row
                .try_get("last_registered_at_ms")
                .map_err(|error| sql_error("decode last registered", error))?,
        })
    }
}

/// Lock one key row for the duration of an admission transaction.
pub(crate) async fn lock_registration_key(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    registration_key_id: &EnvironmentRegistrationKeyId,
) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
    let query = format!(
        "SELECT {KEY_COLUMNS} FROM environment_registration_keys WHERE universe_id = $1 AND registration_key_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&query)
        .bind(universe_id)
        .bind(registration_key_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| sql_error("lock registration key", error))?
        .ok_or_else(|| not_found("environment_registration_key", registration_key_id))?;
    key_from_row(&row)
}

/// The universe whose registration key hashes to `secret_hash`. Key rows are
/// universe-scoped; this is the one lookup that intentionally searches
/// across universes, because the key is what selects the universe.
pub async fn find_registration_key_universe(
    pool: &PgPool,
    secret_hash: &str,
) -> Result<Option<Uuid>, PgStoreError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT universe_id FROM environment_registration_keys WHERE secret_hash = $1",
    )
    .bind(secret_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(universe_id,)| universe_id))
}

/// The universe of the environment bound to a daemon public key, closed
/// rows included. A reconnecting daemon presents only its key, so this is
/// how the gateway finds its universe.
pub async fn find_registered_environment_universe(
    pool: &PgPool,
    daemon_public_key: &str,
) -> Result<Option<Uuid>, PgStoreError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT universe_id FROM environments WHERE daemon_public_key = $1")
            .bind(daemon_public_key)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(universe_id,)| universe_id))
}

fn key_from_row(row: &PgRow) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
    let identity_mode: String = row
        .try_get("identity_mode")
        .map_err(|error| sql_error("decode identity mode", error))?;
    let max_active: Option<i32> = row
        .try_get("max_active_environments")
        .map_err(|error| sql_error("decode max active", error))?;
    let grace: Option<i64> = row
        .try_get("ephemeral_disconnect_grace_ms")
        .map_err(|error| sql_error("decode disconnect grace", error))?;
    let record = EnvironmentRegistrationKeyRecord {
        registration_key_id: EnvironmentRegistrationKeyId::try_new(
            row.try_get::<String, _>("registration_key_id")
                .map_err(|error| sql_error("decode registration key id", error))?,
        )
        .map_err(|error| store_message(format!("decode registration key id: {error}")))?,
        display_name: row
            .try_get("display_name")
            .map_err(|error| sql_error("decode display name", error))?,
        key_prefix: row
            .try_get("key_prefix")
            .map_err(|error| sql_error("decode key prefix", error))?,
        identity_mode: identity_mode_from_str(&identity_mode)?,
        max_active_environments: max_active
            .map(|value| u32::try_from(value).map_err(|_| store_message("max active out of range")))
            .transpose()?,
        ephemeral_disconnect_grace_ms: grace
            .map(|value| u64::try_from(value).map_err(|_| store_message("grace out of range")))
            .transpose()?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|error| sql_error("decode expires", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| sql_error("decode created", error))?,
        revoked_at_ms: row
            .try_get("revoked_at_ms")
            .map_err(|error| sql_error("decode revoked", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn map_key_insert_error(
    error: sqlx::Error,
    registration_key_id: &EnvironmentRegistrationKeyId,
) -> EnvironmentRegistryError {
    if error
        .as_database_error()
        .is_some_and(|db| db.code().as_deref() == Some("23505"))
    {
        return EnvironmentRegistryError::AlreadyExists {
            kind: "environment_registration_key",
            id: registration_key_id.to_string(),
        };
    }
    sql_error("insert registration key", error)
}
