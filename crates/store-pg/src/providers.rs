//! Generic auth provider configurations (P69 G5).
//!
//! One table serves every provider kind. Non-secret config is stored as
//! tagged JSON and decoded into the typed `AuthProviderConfig` enum on read;
//! the credential reference is a typed column with a foreign key into
//! `auth_secrets` (ON DELETE RESTRICT), so a provider's private key cannot
//! be deleted out from under it.

use async_trait::async_trait;
use auth::{
    AuthProviderConfig, AuthProviderId, AuthProviderRecord, AuthProviderStatus, AuthProviderStore,
    AuthRegistryError, CreateAuthProviderRecord, SecretId,
};
use sqlx::Row;

use crate::PgStore;
use crate::auth::{auth_sql_error, auth_store_error, provider_kind_to_str};

const AUTH_PROVIDER_COLUMNS: &str = r#"
    provider_id,
    provider_kind,
    display_name,
    config_json,
    credential_secret_id,
    status,
    created_at_ms,
    updated_at_ms
"#;

#[async_trait]
impl AuthProviderStore for PgStore {
    async fn create_auth_provider(
        &self,
        record: CreateAuthProviderRecord,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| auth_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let config_json = record.config.to_json()?;
        let query = format!(
            r#"
            INSERT INTO auth_providers (
                universe_id,
                provider_id,
                provider_kind,
                display_name,
                config_json,
                credential_secret_id,
                status,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
            ON CONFLICT (universe_id, provider_id) DO NOTHING
            RETURNING {AUTH_PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.provider_id.as_str())
            .bind(provider_kind_to_str(record.provider_kind))
            .bind(record.display_name.as_deref())
            .bind(&config_json)
            .bind(record.credential_secret.as_ref().map(SecretId::as_str))
            .bind(provider_status_to_str(record.status))
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("create auth provider", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::ProviderAlreadyExists {
                provider_id: record.provider_id,
            });
        };
        auth_provider_from_row(&row)
    }

    async fn read_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {AUTH_PROVIDER_COLUMNS}
            FROM auth_providers
            WHERE universe_id = $1 AND provider_id = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("read auth provider", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            });
        };
        auth_provider_from_row(&row)
    }

    async fn list_auth_providers(&self) -> Result<Vec<AuthProviderRecord>, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {AUTH_PROVIDER_COLUMNS}
            FROM auth_providers
            WHERE universe_id = $1
            ORDER BY provider_id
            "#
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| auth_sql_error("list auth providers", error))?;
        rows.iter().map(auth_provider_from_row).collect()
    }

    async fn delete_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        let query = format!(
            r#"
            DELETE FROM auth_providers
            WHERE universe_id = $1 AND provider_id = $2
            RETURNING {AUTH_PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("delete auth provider", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            });
        };
        auth_provider_from_row(&row)
    }
}

fn provider_status_to_str(value: AuthProviderStatus) -> &'static str {
    match value {
        AuthProviderStatus::Active => "active",
        AuthProviderStatus::NeedsConfiguration => "needs_configuration",
        AuthProviderStatus::Disabled => "disabled",
    }
}

fn provider_status_from_str(value: &str) -> Result<AuthProviderStatus, AuthRegistryError> {
    match value {
        "active" => Ok(AuthProviderStatus::Active),
        "needs_configuration" => Ok(AuthProviderStatus::NeedsConfiguration),
        "disabled" => Ok(AuthProviderStatus::Disabled),
        other => Err(AuthRegistryError::Store {
            message: format!("unsupported auth provider status '{other}'"),
        }),
    }
}

fn auth_provider_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<AuthProviderRecord, AuthRegistryError> {
    let provider_id: String = row
        .try_get("provider_id")
        .map_err(|error| auth_sql_error("decode auth provider id", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| auth_sql_error("decode auth provider status", error))?;
    let config_json: serde_json::Value = row
        .try_get("config_json")
        .map_err(|error| auth_sql_error("decode auth provider config", error))?;
    let credential_secret_id: Option<String> = row
        .try_get("credential_secret_id")
        .map_err(|error| auth_sql_error("decode auth provider credential id", error))?;

    let config = AuthProviderConfig::from_json(&config_json)?;
    let record = AuthProviderRecord {
        provider_id: AuthProviderId::try_new(provider_id).map_err(|error| {
            AuthRegistryError::Store {
                message: format!("decode auth provider id: {error}"),
            }
        })?,
        provider_kind: config.provider_kind(),
        display_name: row
            .try_get("display_name")
            .map_err(|error| auth_sql_error("decode auth provider display name", error))?,
        config,
        credential_secret: credential_secret_id
            .map(SecretId::try_new)
            .transpose()
            .map_err(|error| AuthRegistryError::Store {
                message: format!("decode auth provider credential id: {error}"),
            })?,
        status: provider_status_from_str(&status)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| auth_sql_error("decode auth provider created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| auth_sql_error("decode auth provider updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}
