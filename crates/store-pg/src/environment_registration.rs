//! Registration keys for outbound `envd` registration and their secrets,
//! plus the two deployment-wide lookups the environment gateway needs
//! before it knows a universe: a presented secret selects the universe of
//! its key, and a known daemon public key selects the universe of its
//! environment.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use async_trait::async_trait;
use environments::{
    CreateEnvironmentRegistrationKey, EnvironmentRegistrationKeyId,
    EnvironmentRegistrationKeyRecord, EnvironmentRegistrationKeyStore, EnvironmentRegistryError,
    RegistrationKeyUsage, RevokeEnvironmentRegistrationKey, RotateEnvironmentRegistrationKey,
    UpdateEnvironmentRegistrationKey,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    PgStore, PgStoreError,
    environment::{
        constraint_of, decode_rows, identity_mode_from_str, invalid, not_found, sql_error,
        store_message,
    },
};

/// Columns of `environment_registration_keys k`. The display prefix is the
/// live secret's (the latest one when a key has none live).
const KEY_COLUMNS: &str = r#"
    k.registration_key_id, k.display_name, k.identity_mode,
    k.max_active_environments, k.ephemeral_disconnect_grace_ms, k.offline_close_after_ms,
    k.expires_at_ms, k.created_at_ms, k.updated_at_ms, k.revoked_at_ms,
    (
        SELECT s.key_prefix FROM environment_registration_key_secrets s
        WHERE s.universe_id = k.universe_id AND s.registration_key_id = k.registration_key_id
        ORDER BY s.retired_at_ms IS NOT NULL, s.created_at_ms DESC
        LIMIT 1
    ) AS key_prefix
"#;

#[async_trait]
impl EnvironmentRegistrationKeyStore for PgStore {
    async fn create_registration_key(
        &self,
        request: CreateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        request.record.validate()?;
        validate_secret_hash(&request.secret_hash)?;
        self.ensure_universe()
            .await
            .map_err(|error| store_message(format!("ensure universe: {error}")))?;
        let record = request.record;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin registration key create", error))?;
        sqlx::query(
            r#"
            INSERT INTO environment_registration_keys (
                universe_id, registration_key_id, display_name, identity_mode,
                max_active_environments, ephemeral_disconnect_grace_ms, offline_close_after_ms,
                expires_at_ms, revoked_at_ms, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.registration_key_id.as_str())
        .bind(&record.display_name)
        .bind(record.identity_mode.as_str())
        .bind(max_active_column(record.max_active_environments))
        .bind(duration_column(record.ephemeral_disconnect_grace_ms))
        .bind(duration_column(record.offline_close_after_ms))
        .bind(record.expires_at_ms)
        .bind(record.revoked_at_ms)
        .bind(record.created_at_ms)
        .bind(record.updated_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| map_key_write_error(error, &record.registration_key_id))?;
        insert_secret(
            &mut tx,
            self.config.universe_id,
            &record.registration_key_id,
            &request.secret_hash,
            &record.key_prefix,
            record.created_at_ms,
        )
        .await?;
        let record = lock_registration_key(
            &mut tx,
            self.config.universe_id,
            &record.registration_key_id,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit registration key create", error))?;
        Ok(record)
    }

    async fn read_registration_key(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {KEY_COLUMNS} FROM environment_registration_keys k WHERE k.universe_id = $1 AND k.registration_key_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
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
            "SELECT {KEY_COLUMNS} FROM environment_registration_keys k WHERE k.universe_id = $1 ORDER BY k.created_at_ms, k.registration_key_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list registration keys", error))?;
        Ok(decode_rows(
            &rows,
            key_from_row,
            "environment registration key",
        ))
    }

    async fn update_registration_key(
        &self,
        request: UpdateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin registration key update", error))?;
        let mut record = lock_registration_key(
            &mut tx,
            self.config.universe_id,
            &request.registration_key_id,
        )
        .await?;
        request.apply(&mut record)?;
        sqlx::query(
            r#"
            UPDATE environment_registration_keys SET
                display_name = $3, max_active_environments = $4,
                ephemeral_disconnect_grace_ms = $5, offline_close_after_ms = $6,
                expires_at_ms = $7, updated_at_ms = $8
            WHERE universe_id = $1 AND registration_key_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.registration_key_id.as_str())
        .bind(&record.display_name)
        .bind(max_active_column(record.max_active_environments))
        .bind(duration_column(record.ephemeral_disconnect_grace_ms))
        .bind(duration_column(record.offline_close_after_ms))
        .bind(record.expires_at_ms)
        .bind(record.updated_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("update registration key", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit registration key update", error))?;
        Ok(record)
    }

    async fn rotate_registration_key(
        &self,
        request: RotateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin registration key rotation", error))?;
        let mut record = lock_registration_key(
            &mut tx,
            self.config.universe_id,
            &request.registration_key_id,
        )
        .await?;
        request.apply(&mut record)?;
        // Retire before inserting: a key has at most one live secret.
        sqlx::query(
            r#"
            UPDATE environment_registration_key_secrets
            SET retired_at_ms = GREATEST(created_at_ms, $3)
            WHERE universe_id = $1 AND registration_key_id = $2 AND retired_at_ms IS NULL
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.registration_key_id.as_str())
        .bind(request.rotated_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("retire registration key secret", error))?;
        insert_secret(
            &mut tx,
            self.config.universe_id,
            &record.registration_key_id,
            &request.secret_hash,
            &request.key_prefix,
            request.rotated_at_ms,
        )
        .await?;
        sqlx::query(
            "UPDATE environment_registration_keys SET updated_at_ms = $3 WHERE universe_id = $1 AND registration_key_id = $2",
        )
        .bind(self.config.universe_id)
        .bind(record.registration_key_id.as_str())
        .bind(record.updated_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("touch rotated registration key", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit registration key rotation", error))?;
        Ok(record)
    }

    async fn revoke_registration_key(
        &self,
        request: RevokeEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        if request.revoked_at_ms < 0 {
            return invalid("revoked_at_ms must be nonnegative");
        }
        let revoked = sqlx::query(
            r#"
            UPDATE environment_registration_keys SET
                revoked_at_ms = COALESCE(revoked_at_ms, GREATEST($3, created_at_ms)),
                updated_at_ms = CASE WHEN revoked_at_ms IS NULL
                    THEN GREATEST(updated_at_ms, $3, created_at_ms)
                    ELSE updated_at_ms END
            WHERE universe_id = $1 AND registration_key_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.registration_key_id.as_str())
        .bind(request.revoked_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|error| sql_error("revoke registration key", error))?;
        if revoked.rows_affected() == 0 {
            return Err(not_found(
                "environment_registration_key",
                &request.registration_key_id,
            ));
        }
        self.read_registration_key(&request.registration_key_id)
            .await
    }

    async fn resolve_registration_key(
        &self,
        secret_hash: &str,
    ) -> Result<Option<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError> {
        let query = format!(
            r#"
            SELECT {KEY_COLUMNS}
            FROM environment_registration_key_secrets live
            JOIN environment_registration_keys k
              ON k.universe_id = live.universe_id
             AND k.registration_key_id = live.registration_key_id
            WHERE live.universe_id = $1 AND live.secret_hash = $2 AND live.retired_at_ms IS NULL
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
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
                   count(*) FILTER (WHERE state <> 'closed') AS active,
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

/// Lock one key row for the duration of a transaction.
pub(crate) async fn lock_registration_key(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    registration_key_id: &EnvironmentRegistrationKeyId,
) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
    let query = format!(
        "SELECT {KEY_COLUMNS} FROM environment_registration_keys k WHERE k.universe_id = $1 AND k.registration_key_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(universe_id)
        .bind(registration_key_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| sql_error("lock registration key", error))?
        .ok_or_else(|| not_found("environment_registration_key", registration_key_id))?;
    key_from_row(&row)
}

async fn insert_secret(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    registration_key_id: &EnvironmentRegistrationKeyId,
    secret_hash: &str,
    key_prefix: &str,
    created_at_ms: i64,
) -> Result<(), EnvironmentRegistryError> {
    sqlx::query(
        r#"
        INSERT INTO environment_registration_key_secrets (
            secret_hash, universe_id, registration_key_id, key_prefix, created_at_ms
        ) VALUES ($1,$2,$3,$4,$5)
        "#,
    )
    .bind(secret_hash)
    .bind(universe_id)
    .bind(registration_key_id.as_str())
    .bind(key_prefix)
    .bind(created_at_ms)
    .execute(&mut **tx)
    .await
    .map_err(|error| map_key_write_error(error, registration_key_id))?;
    Ok(())
}

/// The universe whose live registration secret hashes to `secret_hash`.
/// Key rows are universe-scoped; this is the one lookup that intentionally
/// searches across universes, because the key is what selects the universe.
/// A retired secret selects nothing.
pub async fn find_registration_key_universe(
    pool: &PgPool,
    secret_hash: &str,
) -> Result<Option<Uuid>, PgStoreError> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT k.universe_id
        FROM environment_registration_key_secrets s
        JOIN environment_registration_keys k
          ON k.universe_id = s.universe_id AND k.registration_key_id = s.registration_key_id
        WHERE s.secret_hash = $1 AND s.retired_at_ms IS NULL
        "#,
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
            .try_get::<Option<String>, _>("key_prefix")
            .map_err(|error| sql_error("decode key prefix", error))?
            .ok_or_else(|| store_message("registration key has no secret"))?,
        identity_mode: identity_mode_from_str(&identity_mode)?,
        max_active_environments: max_active
            .map(|value| u32::try_from(value).map_err(|_| store_message("max active out of range")))
            .transpose()?,
        ephemeral_disconnect_grace_ms: duration_from_row(row, "ephemeral_disconnect_grace_ms")?,
        offline_close_after_ms: duration_from_row(row, "offline_close_after_ms")?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|error| sql_error("decode expires", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| sql_error("decode created", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| sql_error("decode updated", error))?,
        revoked_at_ms: row
            .try_get("revoked_at_ms")
            .map_err(|error| sql_error("decode revoked", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn duration_from_row(row: &PgRow, name: &str) -> Result<Option<u64>, EnvironmentRegistryError> {
    row.try_get::<Option<i64>, _>(name)
        .map_err(|error| sql_error(&format!("decode {name}"), error))?
        .map(|value| {
            u64::try_from(value).map_err(|_| store_message(format!("{name} out of range")))
        })
        .transpose()
}

fn max_active_column(value: Option<u32>) -> Option<i32> {
    value.map(|value| i32::try_from(value).unwrap_or(i32::MAX))
}

fn duration_column(value: Option<u64>) -> Option<i64> {
    value.map(|value| i64::try_from(value).unwrap_or(i64::MAX))
}

/// A registration secret hash is lowercase hex SHA-256.
fn validate_secret_hash(value: &str) -> Result<(), EnvironmentRegistryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid("registration secret hash must be 64 lowercase hex characters");
    }
    Ok(())
}

fn map_key_write_error(
    error: sqlx::Error,
    registration_key_id: &EnvironmentRegistrationKeyId,
) -> EnvironmentRegistryError {
    match constraint_of(&error) {
        Some("environment_registration_keys_pkey") => EnvironmentRegistryError::AlreadyExists {
            kind: "environment_registration_key",
            id: registration_key_id.to_string(),
        },
        Some("environment_registration_key_secrets_pkey") => {
            EnvironmentRegistryError::AlreadyExists {
                kind: "environment_registration_key_secret",
                id: registration_key_id.to_string(),
            }
        }
        _ => sql_error("write registration key", error),
    }
}
