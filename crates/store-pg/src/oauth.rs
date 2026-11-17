//! OAuth client configurations, authorization flows, and the per-grant
//! refresh lock.
//!
//! Tables never hold secret values: client secrets and PKCE verifiers live in
//! `credentials_secrets`, flows store only the SHA-256 hash of the `state`
//! parameter. The refresh lock is a Postgres transaction-scoped advisory
//! lock keyed by universe and grant, so refresh single-flight holds across
//! worker processes.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use api::Attribution;
use async_trait::async_trait;
use credentials::{
    CreateCredentialFlowRecord, CreateOAuthClientRecord, CredentialFlowId, CredentialFlowRecord,
    CredentialFlowStore, CredentialGrantId, CredentialRegistryError, FinishCredentialFlow,
    GrantLockGuard, GrantRefreshLock, OAuthClientId, OAuthClientRecord, OAuthClientStore, SecretId,
    TokenEndpointAuthMethod,
};
use sqlx::Row;

use crate::PgStore;
use crate::credentials::{
    credential_sql_error, credential_store_error, grant_exposure_from_str, grant_exposure_to_str,
    provider_kind_from_str, provider_kind_to_str,
};

const OAUTH_CLIENT_COLUMNS: &str = r#"
    client_id,
    provider_id,
    provider_kind,
    display_name,
    authorization_endpoint,
    token_endpoint,
    remote_client_id,
    client_secret_secret_id,
    token_endpoint_auth_method,
    scopes_default,
    audience,
    authorization_server_issuer,
    authorization_response_iss_parameter_supported,
    authorization_server_scopes_supported,
    created_at_ms,
    updated_at_ms
"#;

const CREDENTIAL_FLOW_COLUMNS: &str = r#"
    flow_id,
    client_id,
    provider_id,
    provider_kind,
    grant_exposure,
    created_by,
    state_hash,
    pkce_verifier_secret_id,
    redirect_uri,
    scopes,
    audience,
    expected_issuer,
    require_issuer,
    grant_id,
    error,
    expires_at_ms,
    consumed_at_ms,
    completed_at_ms,
    created_at_ms,
    updated_at_ms
"#;

#[async_trait]
impl OAuthClientStore for PgStore {
    async fn create_oauth_client(
        &self,
        record: CreateOAuthClientRecord,
    ) -> Result<OAuthClientRecord, CredentialRegistryError> {
        self.require_active_universe()
            .await
            .map_err(|error| credential_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let query = format!(
            r#"
            INSERT INTO credentials_clients (
                universe_id,
                client_id,
                provider_id,
                provider_kind,
                display_name,
                authorization_endpoint,
                token_endpoint,
                remote_client_id,
                client_secret_secret_id,
                token_endpoint_auth_method,
                scopes_default,
                audience,
                authorization_server_issuer,
                authorization_response_iss_parameter_supported,
                authorization_server_scopes_supported,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16)
            ON CONFLICT (universe_id, client_id) DO NOTHING
            RETURNING {OAUTH_CLIENT_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(record.client_id.as_str())
            .bind(&record.provider_id)
            .bind(provider_kind_to_str(record.provider_kind))
            .bind(record.display_name.as_deref())
            .bind(&record.authorization_endpoint)
            .bind(&record.token_endpoint)
            .bind(&record.remote_client_id)
            .bind(record.client_secret.as_ref().map(SecretId::as_str))
            .bind(token_endpoint_auth_method_to_str(
                record.token_endpoint_auth_method,
            ))
            .bind(&record.scopes_default)
            .bind(record.audience.as_deref())
            .bind(record.authorization_server_issuer.as_deref())
            .bind(record.authorization_response_iss_parameter_supported)
            .bind(&record.authorization_server_scopes_supported)
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("create oauth client", error))?;

        let Some(row) = row else {
            return Err(CredentialRegistryError::ClientAlreadyExists {
                client_id: record.client_id,
            });
        };
        oauth_client_from_row(&row)
    }

    async fn read_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, CredentialRegistryError> {
        let query = format!(
            r#"
            SELECT {OAUTH_CLIENT_COLUMNS}
            FROM credentials_clients
            WHERE universe_id = $1 AND client_id = $2
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(client_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("read oauth client", error))?;

        let Some(row) = row else {
            return Err(CredentialRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            });
        };
        oauth_client_from_row(&row)
    }

    async fn list_oauth_clients(&self) -> Result<Vec<OAuthClientRecord>, CredentialRegistryError> {
        let query = format!(
            r#"
            SELECT {OAUTH_CLIENT_COLUMNS}
            FROM credentials_clients
            WHERE universe_id = $1
            ORDER BY client_id
            "#
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| credential_sql_error("list oauth clients", error))?;
        rows.iter().map(oauth_client_from_row).collect()
    }

    async fn delete_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, CredentialRegistryError> {
        let query = format!(
            r#"
            DELETE FROM credentials_clients
            WHERE universe_id = $1 AND client_id = $2
            RETURNING {OAUTH_CLIENT_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(client_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("delete oauth client", error))?;

        let Some(row) = row else {
            return Err(CredentialRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            });
        };
        oauth_client_from_row(&row)
    }
}

#[async_trait]
impl CredentialFlowStore for PgStore {
    async fn create_flow(
        &self,
        record: CreateCredentialFlowRecord,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        self.require_active_universe()
            .await
            .map_err(|error| credential_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let query = format!(
            r#"
            INSERT INTO credentials_flows (
                universe_id,
                flow_id,
                client_id,
                provider_id,
                provider_kind,
                grant_exposure,
                created_by,
                state_hash,
                pkce_verifier_secret_id,
                redirect_uri,
                scopes,
                audience,
                expected_issuer,
                require_issuer,
                expires_at_ms,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16)
            ON CONFLICT (universe_id, flow_id) DO NOTHING
            RETURNING {CREDENTIAL_FLOW_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(record.flow_id.as_str())
            .bind(record.client_id.as_str())
            .bind(&record.provider_id)
            .bind(provider_kind_to_str(record.provider_kind))
            .bind(grant_exposure_to_str(record.grant_exposure))
            .bind(sqlx::types::Json(&record.created_by))
            .bind(&record.state_hash)
            .bind(record.pkce_verifier_secret.as_str())
            .bind(&record.redirect_uri)
            .bind(&record.scopes)
            .bind(record.audience.as_deref())
            .bind(record.expected_issuer.as_deref())
            .bind(record.require_issuer)
            .bind(record.expires_at_ms)
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("create credential flow", error))?;

        let Some(row) = row else {
            return Err(CredentialRegistryError::FlowAlreadyExists {
                flow_id: record.flow_id,
            });
        };
        credential_flow_from_row(&row)
    }

    async fn read_flow(
        &self,
        flow_id: &CredentialFlowId,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        let query = format!(
            r#"
            SELECT {CREDENTIAL_FLOW_COLUMNS}
            FROM credentials_flows
            WHERE universe_id = $1 AND flow_id = $2
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(flow_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("read credential flow", error))?;

        let Some(row) = row else {
            return Err(CredentialRegistryError::FlowNotFound {
                flow_id: flow_id.clone(),
            });
        };
        credential_flow_from_row(&row)
    }

    async fn read_flow_by_state_hash(
        &self,
        state_hash: &str,
    ) -> Result<Option<CredentialFlowRecord>, CredentialRegistryError> {
        let query = format!(
            r#"
            SELECT {CREDENTIAL_FLOW_COLUMNS}
            FROM credentials_flows
            WHERE universe_id = $1 AND state_hash = $2
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(state_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("read credential flow by state", error))?;
        row.as_ref().map(credential_flow_from_row).transpose()
    }

    async fn consume_flow(
        &self,
        flow_id: &CredentialFlowId,
        now_ms: i64,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        let query = format!(
            r#"
            UPDATE credentials_flows
            SET consumed_at_ms = $3, updated_at_ms = $3
            WHERE universe_id = $1
              AND flow_id = $2
              AND consumed_at_ms IS NULL
              AND expires_at_ms > $3
            RETURNING {CREDENTIAL_FLOW_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(flow_id.as_str())
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("consume credential flow", error))?;
        if let Some(row) = row {
            return credential_flow_from_row(&row);
        }

        // The conditional update missed; classify why.
        let current = self.read_flow(flow_id).await?;
        if current.consumed_at_ms.is_some() {
            return Err(CredentialRegistryError::FlowAlreadyConsumed {
                flow_id: flow_id.clone(),
            });
        }
        Err(CredentialRegistryError::FlowExpired {
            flow_id: flow_id.clone(),
        })
    }

    async fn finish_flow(
        &self,
        flow_id: &CredentialFlowId,
        outcome: FinishCredentialFlow,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        outcome.validate()?;
        let query = format!(
            r#"
            UPDATE credentials_flows
            SET grant_id = $3,
                error = $4,
                completed_at_ms = $5,
                updated_at_ms = $5
            WHERE universe_id = $1
              AND flow_id = $2
              AND completed_at_ms IS NULL
            RETURNING {CREDENTIAL_FLOW_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(flow_id.as_str())
            .bind(outcome.grant_id.as_ref().map(CredentialGrantId::as_str))
            .bind(outcome.error.as_deref())
            .bind(outcome.completed_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| credential_sql_error("finish credential flow", error))?;
        if let Some(row) = row {
            return credential_flow_from_row(&row);
        }

        // Missing row vs already-completed row.
        self.read_flow(flow_id).await?;
        Err(CredentialRegistryError::FlowAlreadyCompleted {
            flow_id: flow_id.clone(),
        })
    }
}

#[async_trait]
impl GrantRefreshLock for PgStore {
    async fn lock_grant(
        &self,
        grant_id: &CredentialGrantId,
    ) -> Result<GrantLockGuard, CredentialRegistryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| credential_sql_error("begin grant lock transaction", error))?;
        // Transaction-scoped advisory lock: released when the guarded
        // transaction ends (rollback on guard drop).
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))")
            .bind(self.config.universe_id.to_string())
            .bind(grant_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| credential_sql_error("acquire grant advisory lock", error))?;
        Ok(GrantLockGuard::new(tx))
    }
}

fn token_endpoint_auth_method_to_str(value: TokenEndpointAuthMethod) -> &'static str {
    match value {
        TokenEndpointAuthMethod::ClientSecretBasic => "client_secret_basic",
        TokenEndpointAuthMethod::ClientSecretPost => "client_secret_post",
        TokenEndpointAuthMethod::None => "none",
    }
}

fn token_endpoint_auth_method_from_str(
    value: &str,
) -> Result<TokenEndpointAuthMethod, CredentialRegistryError> {
    match value {
        "client_secret_basic" => Ok(TokenEndpointAuthMethod::ClientSecretBasic),
        "client_secret_post" => Ok(TokenEndpointAuthMethod::ClientSecretPost),
        "none" => Ok(TokenEndpointAuthMethod::None),
        other => Err(CredentialRegistryError::Store {
            message: format!("unsupported token endpoint auth method '{other}'"),
        }),
    }
}

fn oauth_client_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<OAuthClientRecord, CredentialRegistryError> {
    let client_id: String = row
        .try_get("client_id")
        .map_err(|error| credential_sql_error("decode oauth client id", error))?;
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|error| credential_sql_error("decode oauth client provider kind", error))?;
    let client_secret_secret_id: Option<String> = row
        .try_get("client_secret_secret_id")
        .map_err(|error| credential_sql_error("decode oauth client secret id", error))?;
    let token_endpoint_auth_method: String = row
        .try_get("token_endpoint_auth_method")
        .map_err(|error| credential_sql_error("decode oauth client auth method", error))?;

    let record = OAuthClientRecord {
        client_id: OAuthClientId::try_new(client_id).map_err(|error| {
            CredentialRegistryError::Store {
                message: format!("decode oauth client id: {error}"),
            }
        })?,
        provider_id: row
            .try_get("provider_id")
            .map_err(|error| credential_sql_error("decode oauth client provider id", error))?,
        provider_kind: provider_kind_from_str(&provider_kind)?,
        display_name: row
            .try_get("display_name")
            .map_err(|error| credential_sql_error("decode oauth client display name", error))?,
        authorization_endpoint: row.try_get("authorization_endpoint").map_err(|error| {
            credential_sql_error("decode oauth client authorization endpoint", error)
        })?,
        token_endpoint: row
            .try_get("token_endpoint")
            .map_err(|error| credential_sql_error("decode oauth client token endpoint", error))?,
        remote_client_id: row
            .try_get("remote_client_id")
            .map_err(|error| credential_sql_error("decode oauth client remote id", error))?,
        client_secret: client_secret_secret_id
            .map(SecretId::try_new)
            .transpose()
            .map_err(|error| CredentialRegistryError::Store {
                message: format!("decode oauth client secret id: {error}"),
            })?,
        token_endpoint_auth_method: token_endpoint_auth_method_from_str(
            &token_endpoint_auth_method,
        )?,
        scopes_default: row
            .try_get("scopes_default")
            .map_err(|error| credential_sql_error("decode oauth client scopes", error))?,
        audience: row
            .try_get("audience")
            .map_err(|error| credential_sql_error("decode oauth client audience", error))?,
        authorization_server_issuer: row
            .try_get("authorization_server_issuer")
            .map_err(|error| credential_sql_error("decode oauth client issuer", error))?,
        authorization_response_iss_parameter_supported: row
            .try_get("authorization_response_iss_parameter_supported")
            .map_err(|error| credential_sql_error("decode oauth client issuer support", error))?,
        authorization_server_scopes_supported: row
            .try_get("authorization_server_scopes_supported")
            .map_err(|error| credential_sql_error("decode oauth client supported scopes", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| credential_sql_error("decode oauth client created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| credential_sql_error("decode oauth client updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn credential_flow_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<CredentialFlowRecord, CredentialRegistryError> {
    let flow_id: String = row
        .try_get("flow_id")
        .map_err(|error| credential_sql_error("decode credential flow id", error))?;
    let client_id: String = row
        .try_get("client_id")
        .map_err(|error| credential_sql_error("decode credential flow client id", error))?;
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|error| credential_sql_error("decode credential flow provider kind", error))?;
    let grant_exposure: String = row
        .try_get("grant_exposure")
        .map_err(|error| credential_sql_error("decode credential flow grant exposure", error))?;
    let pkce_verifier_secret_id: String =
        row.try_get("pkce_verifier_secret_id").map_err(|error| {
            credential_sql_error("decode credential flow verifier secret id", error)
        })?;
    let grant_id: Option<String> = row
        .try_get("grant_id")
        .map_err(|error| credential_sql_error("decode credential flow grant id", error))?;

    let record = CredentialFlowRecord {
        flow_id: CredentialFlowId::try_new(flow_id).map_err(|error| {
            CredentialRegistryError::Store {
                message: format!("decode credential flow id: {error}"),
            }
        })?,
        client_id: OAuthClientId::try_new(client_id).map_err(|error| {
            CredentialRegistryError::Store {
                message: format!("decode credential flow client id: {error}"),
            }
        })?,
        provider_id: row
            .try_get("provider_id")
            .map_err(|error| credential_sql_error("decode credential flow provider id", error))?,
        provider_kind: provider_kind_from_str(&provider_kind)?,
        grant_exposure: grant_exposure_from_str(&grant_exposure)?,
        created_by: row
            .try_get::<sqlx::types::Json<Attribution>, _>("created_by")
            .map_err(|error| credential_sql_error("decode credential flow creator", error))?
            .0,
        state_hash: row
            .try_get("state_hash")
            .map_err(|error| credential_sql_error("decode credential flow state hash", error))?,
        pkce_verifier_secret: SecretId::try_new(pkce_verifier_secret_id).map_err(|error| {
            CredentialRegistryError::Store {
                message: format!("decode credential flow verifier secret id: {error}"),
            }
        })?,
        redirect_uri: row
            .try_get("redirect_uri")
            .map_err(|error| credential_sql_error("decode credential flow redirect uri", error))?,
        scopes: row
            .try_get("scopes")
            .map_err(|error| credential_sql_error("decode credential flow scopes", error))?,
        audience: row
            .try_get("audience")
            .map_err(|error| credential_sql_error("decode credential flow audience", error))?,
        expected_issuer: row.try_get("expected_issuer").map_err(|error| {
            credential_sql_error("decode credential flow expected issuer", error)
        })?,
        require_issuer: row.try_get("require_issuer").map_err(|error| {
            credential_sql_error("decode credential flow issuer requirement", error)
        })?,
        grant_id: grant_id
            .map(CredentialGrantId::try_new)
            .transpose()
            .map_err(|error| CredentialRegistryError::Store {
                message: format!("decode credential flow grant id: {error}"),
            })?,
        error: row
            .try_get("error")
            .map_err(|error| credential_sql_error("decode credential flow error", error))?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|error| credential_sql_error("decode credential flow expires_at_ms", error))?,
        consumed_at_ms: row.try_get("consumed_at_ms").map_err(|error| {
            credential_sql_error("decode credential flow consumed_at_ms", error)
        })?,
        completed_at_ms: row.try_get("completed_at_ms").map_err(|error| {
            credential_sql_error("decode credential flow completed_at_ms", error)
        })?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| credential_sql_error("decode credential flow created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| credential_sql_error("decode credential flow updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}
