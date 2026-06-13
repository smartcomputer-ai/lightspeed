//! Encrypted secret storage and auth grant records (P69 G1).
//!
//! Secret values are sealed with AES-256-GCM before insertion. The AAD binds
//! each ciphertext to its universe, secret id, and secret kind, so rows cannot
//! be swapped or relabeled without failing decryption. Plaintext exists only
//! inside [`auth_registry::SecretValue`] wrappers in adapter memory.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use async_trait::async_trait;
use auth_registry::{
    AuthGrantId, AuthGrantRecord, AuthGrantStatus, AuthGrantStore, AuthGrantTokenRefresh,
    AuthProviderKind, AuthRegistryError, CreateAuthGrantRecord, ListAuthGrants, OAuthClientId,
    PrincipalKind, PrincipalRef, PutSecretRecord, SecretId, SecretRecordMeta, SecretStore,
    SecretValue,
};
use rand::RngCore;
use sqlx::Row;

use crate::PgStore;

const LOCAL_KEY_ID: &str = "local-v1";
const NONCE_LEN: usize = 12;

impl PgStore {
    fn secrets_cipher(&self) -> Result<(Aes256Gcm, &'static str), AuthRegistryError> {
        let Some(key) = &self.config.secrets_master_key else {
            return Err(AuthRegistryError::Store {
                message: "secrets master key is not configured; set LIGHTSPEED_SECRETS_MASTER_KEY"
                    .to_owned(),
            });
        };
        Ok((
            Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.bytes())),
            LOCAL_KEY_ID,
        ))
    }

    fn secret_aad(&self, secret_id: &SecretId, secret_kind: &str) -> Vec<u8> {
        format!(
            "{}/{}/{}",
            self.config.universe_id,
            secret_id.as_str(),
            secret_kind
        )
        .into_bytes()
    }
}

fn seal_secret(
    cipher: &Aes256Gcm,
    aad: &[u8],
    value: &SecretValue,
) -> Result<(Vec<u8>, Vec<u8>), AuthRegistryError> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: value.expose().as_bytes(),
                aad,
            },
        )
        .map_err(|_| AuthRegistryError::Store {
            message: "secret encryption failed".to_owned(),
        })?;
    Ok((nonce.to_vec(), ciphertext))
}

fn open_secret(
    cipher: &Aes256Gcm,
    aad: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<SecretValue, AuthRegistryError> {
    if nonce.len() != NONCE_LEN {
        return Err(AuthRegistryError::Store {
            message: format!("stored secret nonce has invalid length {}", nonce.len()),
        });
    }
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), Payload {
            msg: ciphertext,
            aad,
        })
        .map_err(|_| AuthRegistryError::Store {
            message: "secret decryption failed; wrong master key or tampered record".to_owned(),
        })?;
    let value = String::from_utf8(plaintext).map_err(|_| AuthRegistryError::Store {
        message: "decrypted secret is not valid UTF-8".to_owned(),
    })?;
    Ok(SecretValue::new(value))
}

#[async_trait]
impl SecretStore for PgStore {
    async fn put_secret(
        &self,
        record: PutSecretRecord,
    ) -> Result<SecretRecordMeta, AuthRegistryError> {
        record.validate()?;
        self.ensure_universe()
            .await
            .map_err(|error| auth_store_error("ensure universe", error))?;
        let (cipher, key_id) = self.secrets_cipher()?;
        let aad = self.secret_aad(&record.secret_id, &record.secret_kind);
        let (nonce, ciphertext) = seal_secret(&cipher, &aad, &record.value)?;

        let row = sqlx::query(
            r#"
            INSERT INTO auth_secrets (
                universe_id,
                secret_id,
                secret_kind,
                key_id,
                nonce,
                ciphertext,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
            ON CONFLICT (universe_id, secret_id) DO NOTHING
            RETURNING secret_id, secret_kind, created_at_ms, updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.secret_id.as_str())
        .bind(&record.secret_kind)
        .bind(key_id)
        .bind(&nonce)
        .bind(&ciphertext)
        .bind(record.created_at_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| auth_sql_error("put secret", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::SecretAlreadyExists {
                secret_id: record.secret_id,
            });
        };
        secret_meta_from_row(&row)
    }

    async fn read_secret(
        &self,
        secret_id: &SecretId,
    ) -> Result<(SecretRecordMeta, SecretValue), AuthRegistryError> {
        let (cipher, _) = self.secrets_cipher()?;
        let row = sqlx::query(
            r#"
            SELECT secret_id, secret_kind, key_id, nonce, ciphertext, created_at_ms, updated_at_ms
            FROM auth_secrets
            WHERE universe_id = $1 AND secret_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(secret_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| auth_sql_error("read secret", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::SecretNotFound {
                secret_id: secret_id.clone(),
            });
        };
        let key_id: String = row
            .try_get("key_id")
            .map_err(|error| auth_sql_error("decode secret key id", error))?;
        if key_id != LOCAL_KEY_ID {
            return Err(AuthRegistryError::Store {
                message: format!("unsupported secret key id '{key_id}'"),
            });
        }
        let meta = secret_meta_from_row(&row)?;
        let nonce: Vec<u8> = row
            .try_get("nonce")
            .map_err(|error| auth_sql_error("decode secret nonce", error))?;
        let ciphertext: Vec<u8> = row
            .try_get("ciphertext")
            .map_err(|error| auth_sql_error("decode secret ciphertext", error))?;
        let aad = self.secret_aad(&meta.secret_id, &meta.secret_kind);
        let value = open_secret(&cipher, &aad, &nonce, &ciphertext)?;
        Ok((meta, value))
    }

    async fn delete_secret(&self, secret_id: &SecretId) -> Result<(), AuthRegistryError> {
        let result = sqlx::query(
            r#"
            DELETE FROM auth_secrets
            WHERE universe_id = $1 AND secret_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(secret_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| auth_sql_error("delete secret", error))?;
        if result.rows_affected() == 0 {
            return Err(AuthRegistryError::SecretNotFound {
                secret_id: secret_id.clone(),
            });
        }
        Ok(())
    }
}

const GRANT_COLUMNS: &str = r#"
    grant_id,
    provider_id,
    provider_kind,
    principal_kind,
    principal_id,
    display_name,
    subject_hint,
    scopes,
    audience,
    access_token_secret_id,
    refresh_token_secret_id,
    oauth_client_id,
    expires_at_ms,
    status,
    metadata_json,
    created_at_ms,
    updated_at_ms
"#;

#[async_trait]
impl AuthGrantStore for PgStore {
    async fn create_grant(
        &self,
        record: CreateAuthGrantRecord,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| auth_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let query = format!(
            r#"
            INSERT INTO auth_grants (
                universe_id,
                grant_id,
                provider_id,
                provider_kind,
                principal_kind,
                principal_id,
                display_name,
                subject_hint,
                scopes,
                audience,
                access_token_secret_id,
                refresh_token_secret_id,
                oauth_client_id,
                expires_at_ms,
                status,
                metadata_json,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $17)
            ON CONFLICT (universe_id, grant_id) DO NOTHING
            RETURNING {GRANT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.grant_id.as_str())
            .bind(&record.provider_id)
            .bind(provider_kind_to_str(record.provider_kind))
            .bind(principal_kind_to_str(record.principal.kind))
            .bind(record.principal.id.as_deref())
            .bind(record.display_name.as_deref())
            .bind(record.subject_hint.as_deref())
            .bind(&record.scopes)
            .bind(record.audience.as_deref())
            .bind(record.access_token_secret.as_ref().map(SecretId::as_str))
            .bind(record.refresh_token_secret.as_ref().map(SecretId::as_str))
            .bind(record.oauth_client.as_ref().map(OAuthClientId::as_str))
            .bind(record.expires_at_ms)
            .bind(grant_status_to_str(record.status))
            .bind(&record.metadata)
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("create auth grant", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::GrantAlreadyExists {
                grant_id: record.grant_id,
            });
        };
        grant_record_from_row(&row)
    }

    async fn read_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {GRANT_COLUMNS}
            FROM auth_grants
            WHERE universe_id = $1 AND grant_id = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(grant_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("read auth grant", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            });
        };
        grant_record_from_row(&row)
    }

    async fn list_grants(
        &self,
        request: ListAuthGrants,
    ) -> Result<Vec<AuthGrantRecord>, AuthRegistryError> {
        let rows = match request.status {
            Some(status) => {
                let query = format!(
                    r#"
                    SELECT {GRANT_COLUMNS}
                    FROM auth_grants
                    WHERE universe_id = $1 AND status = $2
                    ORDER BY grant_id
                    "#
                );
                sqlx::query(&query)
                    .bind(self.config.universe_id)
                    .bind(grant_status_to_str(status))
                    .fetch_all(&self.pool)
                    .await
            }
            None => {
                let query = format!(
                    r#"
                    SELECT {GRANT_COLUMNS}
                    FROM auth_grants
                    WHERE universe_id = $1
                    ORDER BY grant_id
                    "#
                );
                sqlx::query(&query)
                    .bind(self.config.universe_id)
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(|error| auth_sql_error("list auth grants", error))?;

        rows.iter().map(grant_record_from_row).collect()
    }

    async fn update_grant_status(
        &self,
        grant_id: &AuthGrantId,
        status: AuthGrantStatus,
        updated_at_ms: i64,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let query = format!(
            r#"
            UPDATE auth_grants
            SET status = $3, updated_at_ms = $4
            WHERE universe_id = $1 AND grant_id = $2
            RETURNING {GRANT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(grant_id.as_str())
            .bind(grant_status_to_str(status))
            .bind(updated_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("update auth grant status", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            });
        };
        grant_record_from_row(&row)
    }

    async fn record_grant_refresh(
        &self,
        grant_id: &AuthGrantId,
        refresh: AuthGrantTokenRefresh,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let query = format!(
            r#"
            UPDATE auth_grants
            SET access_token_secret_id = $3,
                refresh_token_secret_id = COALESCE($4, refresh_token_secret_id),
                expires_at_ms = $5,
                updated_at_ms = $6
            WHERE universe_id = $1 AND grant_id = $2
            RETURNING {GRANT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(grant_id.as_str())
            .bind(refresh.access_token_secret.as_str())
            .bind(refresh.refresh_token_secret.as_ref().map(SecretId::as_str))
            .bind(refresh.expires_at_ms)
            .bind(refresh.updated_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("record auth grant refresh", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            });
        };
        grant_record_from_row(&row)
    }

    async fn delete_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let query = format!(
            r#"
            DELETE FROM auth_grants
            WHERE universe_id = $1 AND grant_id = $2
            RETURNING {GRANT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(grant_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("delete auth grant", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            });
        };
        grant_record_from_row(&row)
    }
}

fn secret_meta_from_row(row: &sqlx::postgres::PgRow) -> Result<SecretRecordMeta, AuthRegistryError> {
    let secret_id: String = row
        .try_get("secret_id")
        .map_err(|error| auth_sql_error("decode secret id", error))?;
    Ok(SecretRecordMeta {
        secret_id: SecretId::try_new(secret_id).map_err(|error| AuthRegistryError::Store {
            message: format!("decode secret id: {error}"),
        })?,
        secret_kind: row
            .try_get("secret_kind")
            .map_err(|error| auth_sql_error("decode secret kind", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| auth_sql_error("decode secret created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| auth_sql_error("decode secret updated_at_ms", error))?,
    })
}

fn grant_record_from_row(row: &sqlx::postgres::PgRow) -> Result<AuthGrantRecord, AuthRegistryError> {
    let grant_id: String = row
        .try_get("grant_id")
        .map_err(|error| auth_sql_error("decode grant id", error))?;
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|error| auth_sql_error("decode grant provider kind", error))?;
    let principal_kind: String = row
        .try_get("principal_kind")
        .map_err(|error| auth_sql_error("decode grant principal kind", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| auth_sql_error("decode grant status", error))?;
    let access_token_secret_id: Option<String> = row
        .try_get("access_token_secret_id")
        .map_err(|error| auth_sql_error("decode grant access token secret id", error))?;
    let refresh_token_secret_id: Option<String> = row
        .try_get("refresh_token_secret_id")
        .map_err(|error| auth_sql_error("decode grant refresh token secret id", error))?;
    let oauth_client_id: Option<String> = row
        .try_get("oauth_client_id")
        .map_err(|error| auth_sql_error("decode grant oauth client id", error))?;

    let record = AuthGrantRecord {
        grant_id: AuthGrantId::try_new(grant_id).map_err(|error| AuthRegistryError::Store {
            message: format!("decode grant id: {error}"),
        })?,
        provider_id: row
            .try_get("provider_id")
            .map_err(|error| auth_sql_error("decode grant provider id", error))?,
        provider_kind: provider_kind_from_str(&provider_kind)?,
        principal: PrincipalRef {
            kind: principal_kind_from_str(&principal_kind)?,
            id: row
                .try_get("principal_id")
                .map_err(|error| auth_sql_error("decode grant principal id", error))?,
        },
        display_name: row
            .try_get("display_name")
            .map_err(|error| auth_sql_error("decode grant display name", error))?,
        subject_hint: row
            .try_get("subject_hint")
            .map_err(|error| auth_sql_error("decode grant subject hint", error))?,
        scopes: row
            .try_get("scopes")
            .map_err(|error| auth_sql_error("decode grant scopes", error))?,
        audience: row
            .try_get("audience")
            .map_err(|error| auth_sql_error("decode grant audience", error))?,
        access_token_secret: access_token_secret_id
            .map(SecretId::try_new)
            .transpose()
            .map_err(|error| AuthRegistryError::Store {
                message: format!("decode grant access token secret id: {error}"),
            })?,
        refresh_token_secret: refresh_token_secret_id
            .map(SecretId::try_new)
            .transpose()
            .map_err(|error| AuthRegistryError::Store {
                message: format!("decode grant refresh token secret id: {error}"),
            })?,
        oauth_client: oauth_client_id
            .map(OAuthClientId::try_new)
            .transpose()
            .map_err(|error| AuthRegistryError::Store {
                message: format!("decode grant oauth client id: {error}"),
            })?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|error| auth_sql_error("decode grant expires_at_ms", error))?,
        status: grant_status_from_str(&status)?,
        metadata: row
            .try_get("metadata_json")
            .map_err(|error| auth_sql_error("decode grant metadata", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| auth_sql_error("decode grant created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| auth_sql_error("decode grant updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

pub(crate) fn provider_kind_to_str(value: AuthProviderKind) -> &'static str {
    match value {
        AuthProviderKind::StaticBearer => "static_bearer",
        AuthProviderKind::McpOAuth => "mcp_oauth",
        AuthProviderKind::GitHubApp => "github_app",
        AuthProviderKind::GitHubAppUser => "github_app_user",
        AuthProviderKind::GitHubOAuthApp => "github_oauth_app",
        AuthProviderKind::CustomOAuth => "custom_oauth",
        AuthProviderKind::ModelApiKey => "model_api_key",
        AuthProviderKind::ModelOAuth => "model_oauth",
    }
}

pub(crate) fn provider_kind_from_str(value: &str) -> Result<AuthProviderKind, AuthRegistryError> {
    match value {
        "static_bearer" => Ok(AuthProviderKind::StaticBearer),
        "mcp_oauth" => Ok(AuthProviderKind::McpOAuth),
        "github_app" => Ok(AuthProviderKind::GitHubApp),
        "github_app_user" => Ok(AuthProviderKind::GitHubAppUser),
        "github_oauth_app" => Ok(AuthProviderKind::GitHubOAuthApp),
        "custom_oauth" => Ok(AuthProviderKind::CustomOAuth),
        "model_api_key" => Ok(AuthProviderKind::ModelApiKey),
        "model_oauth" => Ok(AuthProviderKind::ModelOAuth),
        other => Err(AuthRegistryError::Store {
            message: format!("unsupported auth provider kind '{other}'"),
        }),
    }
}

pub(crate) fn principal_kind_to_str(value: PrincipalKind) -> &'static str {
    match value {
        PrincipalKind::User => "user",
        PrincipalKind::ServiceAccount => "service_account",
        PrincipalKind::UniverseDefault => "universe_default",
    }
}

pub(crate) fn principal_kind_from_str(value: &str) -> Result<PrincipalKind, AuthRegistryError> {
    match value {
        "user" => Ok(PrincipalKind::User),
        "service_account" => Ok(PrincipalKind::ServiceAccount),
        "universe_default" => Ok(PrincipalKind::UniverseDefault),
        other => Err(AuthRegistryError::Store {
            message: format!("unsupported auth principal kind '{other}'"),
        }),
    }
}

fn grant_status_to_str(value: AuthGrantStatus) -> &'static str {
    match value {
        AuthGrantStatus::Active => "active",
        AuthGrantStatus::NeedsReauth => "needs_reauth",
        AuthGrantStatus::Revoked => "revoked",
        AuthGrantStatus::Failed => "failed",
    }
}

fn grant_status_from_str(value: &str) -> Result<AuthGrantStatus, AuthRegistryError> {
    match value {
        "active" => Ok(AuthGrantStatus::Active),
        "needs_reauth" => Ok(AuthGrantStatus::NeedsReauth),
        "revoked" => Ok(AuthGrantStatus::Revoked),
        "failed" => Ok(AuthGrantStatus::Failed),
        other => Err(AuthRegistryError::Store {
            message: format!("unsupported auth grant status '{other}'"),
        }),
    }
}

pub(crate) fn auth_store_error(action: &str, error: crate::PgStoreError) -> AuthRegistryError {
    AuthRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

pub(crate) fn auth_sql_error(action: &str, error: sqlx::Error) -> AuthRegistryError {
    AuthRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}
