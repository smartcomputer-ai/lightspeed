//! Generic auth provider configurations.
//!
//! One table serves every provider kind. Non-secret config is stored as
//! tagged JSON and decoded into the typed `AuthProviderConfig` enum on read;
//! the credential reference is a typed column with a foreign key into
//! `auth_secrets` (ON DELETE RESTRICT), so a provider's private key cannot
//! be deleted out from under it. A model endpoint's extra request headers are
//! rows of `auth_provider_headers`, not part of the JSON, so a credential a
//! header names is held by a foreign key too.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use async_trait::async_trait;
use auth::{
    AuthProviderConfig, AuthProviderId, AuthProviderRecord, AuthProviderStatus, AuthProviderStore,
    AuthRegistryError, CreateAuthProviderRecord, RequestHeaderValue, RequestHeaders, SecretId,
};
use sqlx::Row;

use crate::PgStore;
use crate::auth::{auth_sql_error, auth_store_error, provider_kind_to_str};

/// Every column of a provider row plus its endpoint header rows folded into
/// one JSON array, so reads, lists and post-write reads decode the whole
/// record.
const AUTH_PROVIDER_COLUMNS: &str = r#"
    p.provider_id,
    p.provider_kind,
    p.display_name,
    p.config_json,
    p.credential_secret_id,
    p.status,
    p.created_at_ms,
    p.updated_at_ms,
    COALESCE(
        (
            SELECT jsonb_agg(
                jsonb_build_object(
                    'name', h.name,
                    'literal_value', h.literal_value,
                    'grant_id', h.grant_id
                )
                ORDER BY h.name
            )
            FROM auth_provider_headers AS h
            WHERE h.universe_id = p.universe_id AND h.provider_id = p.provider_id
        ),
        '[]'::jsonb
    ) AS headers_json
"#;

const HEADER_GRANT_FK: &str = "auth_provider_headers_grant_fk";

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
        let (config_json, headers) = stored_config(&record.config)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| auth_sql_error("begin auth provider create", error))?;
        let inserted = sqlx::query(
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
            RETURNING provider_id
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.provider_id.as_str())
        .bind(provider_kind_to_str(record.provider_kind))
        .bind(record.display_name.as_deref())
        .bind(&config_json)
        .bind(record.credential_secret.as_ref().map(SecretId::as_str))
        .bind(provider_status_to_str(record.status))
        .bind(record.created_at_ms)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| auth_sql_error("create auth provider", error))?;
        if inserted.is_none() {
            return Err(AuthRegistryError::ProviderAlreadyExists {
                provider_id: record.provider_id,
            });
        }
        self.finish_provider_write(tx, &record.provider_id, &headers)
            .await
    }

    async fn read_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        select_provider(&self.pool, self.config.universe_id, provider_id, false)
            .await?
            .ok_or_else(|| AuthRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            })
    }

    async fn list_auth_providers(&self) -> Result<Vec<AuthProviderRecord>, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {AUTH_PROVIDER_COLUMNS}
            FROM auth_providers AS p
            WHERE p.universe_id = $1
            ORDER BY p.provider_id
            "#
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| auth_sql_error("begin auth provider delete", error))?;
        let Some(record) =
            select_provider(&mut *tx, self.config.universe_id, provider_id, true).await?
        else {
            return Err(AuthRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            });
        };
        // Header rows cascade with the provider.
        sqlx::query("DELETE FROM auth_providers WHERE universe_id = $1 AND provider_id = $2")
            .bind(self.config.universe_id)
            .bind(provider_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| auth_sql_error("delete auth provider", error))?;
        tx.commit()
            .await
            .map_err(|error| auth_sql_error("commit auth provider delete", error))?;
        Ok(record)
    }

    async fn replace_auth_provider(
        &self,
        record: AuthProviderRecord,
        expected_credential: Option<&SecretId>,
    ) -> Result<Option<AuthProviderRecord>, AuthRegistryError> {
        record.validate()?;
        let (config_json, headers) = stored_config(&record.config)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| auth_sql_error("begin auth provider replace", error))?;
        let updated = sqlx::query(
            r#"
            UPDATE auth_providers
            SET provider_kind = $3,
                display_name = $4,
                config_json = $5,
                credential_secret_id = $6,
                status = $7,
                updated_at_ms = GREATEST(updated_at_ms, $8)
            WHERE universe_id = $1 AND provider_id = $2
              AND credential_secret_id IS NOT DISTINCT FROM $9
            RETURNING provider_id
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.provider_id.as_str())
        .bind(provider_kind_to_str(record.provider_kind))
        .bind(record.display_name.as_deref())
        .bind(&config_json)
        .bind(record.credential_secret.as_ref().map(SecretId::as_str))
        .bind(provider_status_to_str(record.status))
        .bind(record.updated_at_ms)
        .bind(expected_credential.map(SecretId::as_str))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| auth_sql_error("replace auth provider", error))?;
        if updated.is_none() {
            drop(tx);
            // Distinguish a lost race from a missing provider.
            self.read_auth_provider(&record.provider_id).await?;
            return Ok(None);
        }
        self.finish_provider_write(tx, &record.provider_id, &headers)
            .await
            .map(Some)
    }
}

impl PgStore {
    /// Replace the provider's header rows, read the whole record back inside
    /// the same transaction, and commit.
    async fn finish_provider_write(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        provider_id: &AuthProviderId,
        headers: &RequestHeaders,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        let universe_id = self.config.universe_id;
        sqlx::query(
            "DELETE FROM auth_provider_headers WHERE universe_id = $1 AND provider_id = $2",
        )
        .bind(universe_id)
        .bind(provider_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| auth_sql_error("clear model endpoint headers", error))?;
        if !headers.is_empty() {
            let mut names = Vec::with_capacity(headers.len());
            let mut literals = Vec::with_capacity(headers.len());
            let mut grants = Vec::with_capacity(headers.len());
            for (name, value) in headers {
                names.push(name.clone());
                match value {
                    RequestHeaderValue::Literal { value } => {
                        literals.push(Some(value.clone()));
                        grants.push(None);
                    }
                    RequestHeaderValue::Credential { grant_id } => {
                        literals.push(None);
                        grants.push(Some(grant_id.as_str().to_owned()));
                    }
                }
            }
            sqlx::query(
                r#"
                INSERT INTO auth_provider_headers (
                    universe_id, provider_id, name, literal_value, grant_id
                )
                SELECT $1, $2, header.name, header.literal_value, header.grant_id
                FROM UNNEST($3::text[], $4::text[], $5::text[])
                    AS header (name, literal_value, grant_id)
                "#,
            )
            .bind(universe_id)
            .bind(provider_id.as_str())
            .bind(&names)
            .bind(&literals)
            .bind(&grants)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                if foreign_key_constraint(&error) == Some(HEADER_GRANT_FK) {
                    AuthRegistryError::InvalidInput {
                        message: "a model endpoint header references a credential that does \
                                  not exist"
                            .to_owned(),
                    }
                } else {
                    auth_sql_error("write model endpoint headers", error)
                }
            })?;
        }
        let written = select_provider(&mut *tx, universe_id, provider_id, false)
            .await?
            .ok_or_else(|| AuthRegistryError::Store {
                message: format!("auth provider {provider_id} vanished during write"),
            })?;
        tx.commit()
            .await
            .map_err(|error| auth_sql_error("commit auth provider write", error))?;
        Ok(written)
    }
}

/// The configuration document as stored, and the endpoint headers that are
/// stored beside it as rows instead.
fn stored_config(
    config: &AuthProviderConfig,
) -> Result<(serde_json::Value, RequestHeaders), AuthRegistryError> {
    let mut document = config.clone();
    let headers = document
        .model_endpoint_mut()
        .map(|endpoint| std::mem::take(&mut endpoint.headers))
        .unwrap_or_default();
    Ok((document.to_json()?, headers))
}

async fn select_provider<'e, E>(
    executor: E,
    universe_id: uuid::Uuid,
    provider_id: &AuthProviderId,
    for_update: bool,
) -> Result<Option<AuthProviderRecord>, AuthRegistryError>
where
    E: sqlx::PgExecutor<'e>,
{
    let lock = if for_update { "FOR UPDATE OF p" } else { "" };
    let query = format!(
        r#"
        SELECT {AUTH_PROVIDER_COLUMNS}
        FROM auth_providers AS p
        WHERE p.universe_id = $1 AND p.provider_id = $2
        {lock}
        "#
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(universe_id)
        .bind(provider_id.as_str())
        .fetch_optional(executor)
        .await
        .map_err(|error| auth_sql_error("read auth provider", error))?;
    row.as_ref().map(auth_provider_from_row).transpose()
}

fn foreign_key_constraint(error: &sqlx::Error) -> Option<&str> {
    let database = error.as_database_error()?;
    if database.code().as_deref() != Some("23503") {
        return None;
    }
    database.constraint()
}

#[derive(serde::Deserialize)]
struct HeaderRow {
    name: String,
    literal_value: Option<String>,
    grant_id: Option<String>,
}

fn headers_from_json(value: serde_json::Value) -> Result<RequestHeaders, AuthRegistryError> {
    let rows: Vec<HeaderRow> =
        serde_json::from_value(value).map_err(|error| AuthRegistryError::Store {
            message: format!("decode model endpoint headers: {error}"),
        })?;
    rows.into_iter()
        .map(|row| {
            let value = match (row.literal_value, row.grant_id) {
                (Some(value), None) => RequestHeaderValue::Literal { value },
                (None, Some(grant_id)) => RequestHeaderValue::Credential {
                    grant_id: auth::AuthGrantId::try_new(grant_id).map_err(|error| {
                        AuthRegistryError::Store {
                            message: format!("decode model endpoint header grant id: {error}"),
                        }
                    })?,
                },
                _ => {
                    return Err(AuthRegistryError::Store {
                        message: format!(
                            "model endpoint header {} has no single value source",
                            row.name
                        ),
                    });
                }
            };
            Ok((row.name, value))
        })
        .collect()
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

    let headers_json: serde_json::Value = row
        .try_get("headers_json")
        .map_err(|error| auth_sql_error("decode model endpoint headers", error))?;

    let mut config = AuthProviderConfig::from_json(&config_json)?;
    let headers = headers_from_json(headers_json)?;
    match config.model_endpoint_mut() {
        Some(endpoint) => endpoint.headers = headers,
        None if headers.is_empty() => {}
        None => {
            return Err(AuthRegistryError::Store {
                message: format!(
                    "auth provider {provider_id} has endpoint headers but no model endpoint"
                ),
            });
        }
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use auth::{AuthGrantId, ModelEndpointConfig, ModelEndpointOnlyConfig};

    use super::*;

    #[test]
    fn stored_config_keeps_headers_out_of_the_document() {
        let headers = BTreeMap::from([
            (
                "x-title".to_owned(),
                RequestHeaderValue::Literal {
                    value: "Lightspeed".to_owned(),
                },
            ),
            (
                "x-api-key".to_owned(),
                RequestHeaderValue::Credential {
                    grant_id: AuthGrantId::new("authgrant_router"),
                },
            ),
        ]);
        let config = AuthProviderConfig::ModelEndpoint(ModelEndpointOnlyConfig {
            endpoint: ModelEndpointConfig {
                base_url: "https://router.example/v1".to_owned(),
                headers: headers.clone(),
                api_kinds: vec!["openai:completions".to_owned()],
            },
        });

        let (document, stored_headers) = stored_config(&config).expect("split config");

        assert_eq!(stored_headers, headers);
        assert!(document["endpoint"].get("headers").is_none());
        // Decoding the document alone yields the endpoint without headers;
        // the rows put them back.
        let mut decoded = AuthProviderConfig::from_json(&document).expect("decode document");
        assert!(
            decoded
                .model_endpoint()
                .expect("endpoint")
                .headers
                .is_empty()
        );
        decoded.model_endpoint_mut().expect("endpoint").headers = stored_headers;
        assert_eq!(decoded, config);
    }

    #[test]
    fn stored_config_of_a_provider_without_an_endpoint_has_no_headers() {
        let config = AuthProviderConfig::ModelApiKey(auth::ModelApiKeyConfig::default());
        let (document, headers) = stored_config(&config).expect("split config");
        assert!(headers.is_empty());
        assert_eq!(document, config.to_json().expect("encode"));
    }

    #[test]
    fn header_rows_decode_to_one_value_source_each() {
        let decoded = headers_from_json(serde_json::json!([
            { "name": "x-api-key", "literal_value": null, "grant_id": "authgrant_router" },
            { "name": "x-title", "literal_value": "Lightspeed", "grant_id": null },
        ]))
        .expect("decode rows");
        assert_eq!(
            decoded.get("x-api-key"),
            Some(&RequestHeaderValue::Credential {
                grant_id: AuthGrantId::new("authgrant_router"),
            })
        );
        assert_eq!(
            decoded.get("x-title"),
            Some(&RequestHeaderValue::Literal {
                value: "Lightspeed".to_owned(),
            })
        );

        for invalid in [
            serde_json::json!([{ "name": "x-a", "literal_value": null, "grant_id": null }]),
            serde_json::json!([{ "name": "x-a", "literal_value": "v", "grant_id": "authgrant_1" }]),
        ] {
            assert!(matches!(
                headers_from_json(invalid),
                Err(AuthRegistryError::Store { .. })
            ));
        }
    }
}
