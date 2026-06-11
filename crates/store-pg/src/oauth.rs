//! OAuth client configurations, authorization flows, and the per-grant
//! refresh lock (P69 G2/G3).
//!
//! Tables never hold secret values: client secrets and PKCE verifiers live in
//! `auth_secrets`, flows store only the SHA-256 hash of the `state`
//! parameter. The refresh lock is a Postgres transaction-scoped advisory
//! lock keyed by universe and grant, so refresh single-flight holds across
//! worker processes.

use async_trait::async_trait;
use auth_registry::{
    AuthFlowId, AuthFlowRecord, AuthFlowStore, AuthGrantId, AuthRegistryError,
    CreateAuthFlowRecord, CreateOAuthClientRecord, FinishAuthFlow, GrantLockGuard,
    GrantRefreshLock, OAuthClientId, OAuthClientRecord, OAuthClientStore, PrincipalRef, SecretId,
    TokenEndpointAuthMethod,
};
use sqlx::Row;

use crate::PgStore;
use crate::auth::{
    auth_sql_error, auth_store_error, principal_kind_from_str, principal_kind_to_str,
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
    created_at_ms,
    updated_at_ms
"#;

const AUTH_FLOW_COLUMNS: &str = r#"
    flow_id,
    client_id,
    provider_id,
    provider_kind,
    principal_kind,
    principal_id,
    state_hash,
    pkce_verifier_secret_id,
    redirect_uri,
    scopes,
    audience,
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
    ) -> Result<OAuthClientRecord, AuthRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| auth_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let query = format!(
            r#"
            INSERT INTO auth_clients (
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
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)
            ON CONFLICT (universe_id, client_id) DO NOTHING
            RETURNING {OAUTH_CLIENT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
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
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("create oauth client", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::ClientAlreadyExists {
                client_id: record.client_id,
            });
        };
        oauth_client_from_row(&row)
    }

    async fn read_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {OAUTH_CLIENT_COLUMNS}
            FROM auth_clients
            WHERE universe_id = $1 AND client_id = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(client_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("read oauth client", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            });
        };
        oauth_client_from_row(&row)
    }

    async fn list_oauth_clients(&self) -> Result<Vec<OAuthClientRecord>, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {OAUTH_CLIENT_COLUMNS}
            FROM auth_clients
            WHERE universe_id = $1
            ORDER BY client_id
            "#
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| auth_sql_error("list oauth clients", error))?;
        rows.iter().map(oauth_client_from_row).collect()
    }

    async fn delete_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, AuthRegistryError> {
        let query = format!(
            r#"
            DELETE FROM auth_clients
            WHERE universe_id = $1 AND client_id = $2
            RETURNING {OAUTH_CLIENT_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(client_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("delete oauth client", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            });
        };
        oauth_client_from_row(&row)
    }
}

#[async_trait]
impl AuthFlowStore for PgStore {
    async fn create_flow(
        &self,
        record: CreateAuthFlowRecord,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| auth_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let query = format!(
            r#"
            INSERT INTO auth_flows (
                universe_id,
                flow_id,
                client_id,
                provider_id,
                provider_kind,
                principal_kind,
                principal_id,
                state_hash,
                pkce_verifier_secret_id,
                redirect_uri,
                scopes,
                audience,
                expires_at_ms,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14)
            ON CONFLICT (universe_id, flow_id) DO NOTHING
            RETURNING {AUTH_FLOW_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.flow_id.as_str())
            .bind(record.client_id.as_str())
            .bind(&record.provider_id)
            .bind(provider_kind_to_str(record.provider_kind))
            .bind(principal_kind_to_str(record.principal.kind))
            .bind(record.principal.id.as_deref())
            .bind(&record.state_hash)
            .bind(record.pkce_verifier_secret.as_str())
            .bind(&record.redirect_uri)
            .bind(&record.scopes)
            .bind(record.audience.as_deref())
            .bind(record.expires_at_ms)
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("create auth flow", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::FlowAlreadyExists {
                flow_id: record.flow_id,
            });
        };
        auth_flow_from_row(&row)
    }

    async fn read_flow(&self, flow_id: &AuthFlowId) -> Result<AuthFlowRecord, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {AUTH_FLOW_COLUMNS}
            FROM auth_flows
            WHERE universe_id = $1 AND flow_id = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(flow_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("read auth flow", error))?;

        let Some(row) = row else {
            return Err(AuthRegistryError::FlowNotFound {
                flow_id: flow_id.clone(),
            });
        };
        auth_flow_from_row(&row)
    }

    async fn read_flow_by_state_hash(
        &self,
        state_hash: &str,
    ) -> Result<Option<AuthFlowRecord>, AuthRegistryError> {
        let query = format!(
            r#"
            SELECT {AUTH_FLOW_COLUMNS}
            FROM auth_flows
            WHERE universe_id = $1 AND state_hash = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(state_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("read auth flow by state", error))?;
        row.as_ref().map(auth_flow_from_row).transpose()
    }

    async fn consume_flow(
        &self,
        flow_id: &AuthFlowId,
        now_ms: i64,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        let query = format!(
            r#"
            UPDATE auth_flows
            SET consumed_at_ms = $3, updated_at_ms = $3, modified_at = now()
            WHERE universe_id = $1
              AND flow_id = $2
              AND consumed_at_ms IS NULL
              AND expires_at_ms > $3
            RETURNING {AUTH_FLOW_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(flow_id.as_str())
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("consume auth flow", error))?;
        if let Some(row) = row {
            return auth_flow_from_row(&row);
        }

        // The conditional update missed; classify why.
        let current = self.read_flow(flow_id).await?;
        if current.consumed_at_ms.is_some() {
            return Err(AuthRegistryError::FlowAlreadyConsumed {
                flow_id: flow_id.clone(),
            });
        }
        Err(AuthRegistryError::FlowExpired {
            flow_id: flow_id.clone(),
        })
    }

    async fn finish_flow(
        &self,
        flow_id: &AuthFlowId,
        outcome: FinishAuthFlow,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        outcome.validate()?;
        let query = format!(
            r#"
            UPDATE auth_flows
            SET grant_id = $3,
                error = $4,
                completed_at_ms = $5,
                updated_at_ms = $5,
                modified_at = now()
            WHERE universe_id = $1
              AND flow_id = $2
              AND completed_at_ms IS NULL
            RETURNING {AUTH_FLOW_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(flow_id.as_str())
            .bind(outcome.grant_id.as_ref().map(AuthGrantId::as_str))
            .bind(outcome.error.as_deref())
            .bind(outcome.completed_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| auth_sql_error("finish auth flow", error))?;
        if let Some(row) = row {
            return auth_flow_from_row(&row);
        }

        // Missing row vs already-completed row.
        self.read_flow(flow_id).await?;
        Err(AuthRegistryError::FlowAlreadyCompleted {
            flow_id: flow_id.clone(),
        })
    }
}

#[async_trait]
impl GrantRefreshLock for PgStore {
    async fn lock_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<GrantLockGuard, AuthRegistryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| auth_sql_error("begin grant lock transaction", error))?;
        // Transaction-scoped advisory lock: released when the guarded
        // transaction ends (rollback on guard drop).
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))")
            .bind(self.config.universe_id.to_string())
            .bind(grant_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| auth_sql_error("acquire grant advisory lock", error))?;
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
) -> Result<TokenEndpointAuthMethod, AuthRegistryError> {
    match value {
        "client_secret_basic" => Ok(TokenEndpointAuthMethod::ClientSecretBasic),
        "client_secret_post" => Ok(TokenEndpointAuthMethod::ClientSecretPost),
        "none" => Ok(TokenEndpointAuthMethod::None),
        other => Err(AuthRegistryError::Store {
            message: format!("unsupported token endpoint auth method '{other}'"),
        }),
    }
}

fn oauth_client_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<OAuthClientRecord, AuthRegistryError> {
    let client_id: String = row
        .try_get("client_id")
        .map_err(|error| auth_sql_error("decode oauth client id", error))?;
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|error| auth_sql_error("decode oauth client provider kind", error))?;
    let client_secret_secret_id: Option<String> = row
        .try_get("client_secret_secret_id")
        .map_err(|error| auth_sql_error("decode oauth client secret id", error))?;
    let token_endpoint_auth_method: String = row
        .try_get("token_endpoint_auth_method")
        .map_err(|error| auth_sql_error("decode oauth client auth method", error))?;

    let record = OAuthClientRecord {
        client_id: OAuthClientId::try_new(client_id).map_err(|error| {
            AuthRegistryError::Store {
                message: format!("decode oauth client id: {error}"),
            }
        })?,
        provider_id: row
            .try_get("provider_id")
            .map_err(|error| auth_sql_error("decode oauth client provider id", error))?,
        provider_kind: provider_kind_from_str(&provider_kind)?,
        display_name: row
            .try_get("display_name")
            .map_err(|error| auth_sql_error("decode oauth client display name", error))?,
        authorization_endpoint: row
            .try_get("authorization_endpoint")
            .map_err(|error| auth_sql_error("decode oauth client authorization endpoint", error))?,
        token_endpoint: row
            .try_get("token_endpoint")
            .map_err(|error| auth_sql_error("decode oauth client token endpoint", error))?,
        remote_client_id: row
            .try_get("remote_client_id")
            .map_err(|error| auth_sql_error("decode oauth client remote id", error))?,
        client_secret: client_secret_secret_id
            .map(SecretId::try_new)
            .transpose()
            .map_err(|error| AuthRegistryError::Store {
                message: format!("decode oauth client secret id: {error}"),
            })?,
        token_endpoint_auth_method: token_endpoint_auth_method_from_str(
            &token_endpoint_auth_method,
        )?,
        scopes_default: row
            .try_get("scopes_default")
            .map_err(|error| auth_sql_error("decode oauth client scopes", error))?,
        audience: row
            .try_get("audience")
            .map_err(|error| auth_sql_error("decode oauth client audience", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| auth_sql_error("decode oauth client created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| auth_sql_error("decode oauth client updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn auth_flow_from_row(row: &sqlx::postgres::PgRow) -> Result<AuthFlowRecord, AuthRegistryError> {
    let flow_id: String = row
        .try_get("flow_id")
        .map_err(|error| auth_sql_error("decode auth flow id", error))?;
    let client_id: String = row
        .try_get("client_id")
        .map_err(|error| auth_sql_error("decode auth flow client id", error))?;
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|error| auth_sql_error("decode auth flow provider kind", error))?;
    let principal_kind: String = row
        .try_get("principal_kind")
        .map_err(|error| auth_sql_error("decode auth flow principal kind", error))?;
    let pkce_verifier_secret_id: String = row
        .try_get("pkce_verifier_secret_id")
        .map_err(|error| auth_sql_error("decode auth flow verifier secret id", error))?;
    let grant_id: Option<String> = row
        .try_get("grant_id")
        .map_err(|error| auth_sql_error("decode auth flow grant id", error))?;

    let record = AuthFlowRecord {
        flow_id: AuthFlowId::try_new(flow_id).map_err(|error| AuthRegistryError::Store {
            message: format!("decode auth flow id: {error}"),
        })?,
        client_id: OAuthClientId::try_new(client_id).map_err(|error| {
            AuthRegistryError::Store {
                message: format!("decode auth flow client id: {error}"),
            }
        })?,
        provider_id: row
            .try_get("provider_id")
            .map_err(|error| auth_sql_error("decode auth flow provider id", error))?,
        provider_kind: provider_kind_from_str(&provider_kind)?,
        principal: PrincipalRef {
            kind: principal_kind_from_str(&principal_kind)?,
            id: row
                .try_get("principal_id")
                .map_err(|error| auth_sql_error("decode auth flow principal id", error))?,
        },
        state_hash: row
            .try_get("state_hash")
            .map_err(|error| auth_sql_error("decode auth flow state hash", error))?,
        pkce_verifier_secret: SecretId::try_new(pkce_verifier_secret_id).map_err(|error| {
            AuthRegistryError::Store {
                message: format!("decode auth flow verifier secret id: {error}"),
            }
        })?,
        redirect_uri: row
            .try_get("redirect_uri")
            .map_err(|error| auth_sql_error("decode auth flow redirect uri", error))?,
        scopes: row
            .try_get("scopes")
            .map_err(|error| auth_sql_error("decode auth flow scopes", error))?,
        audience: row
            .try_get("audience")
            .map_err(|error| auth_sql_error("decode auth flow audience", error))?,
        grant_id: grant_id
            .map(AuthGrantId::try_new)
            .transpose()
            .map_err(|error| AuthRegistryError::Store {
                message: format!("decode auth flow grant id: {error}"),
            })?,
        error: row
            .try_get("error")
            .map_err(|error| auth_sql_error("decode auth flow error", error))?,
        expires_at_ms: row
            .try_get("expires_at_ms")
            .map_err(|error| auth_sql_error("decode auth flow expires_at_ms", error))?,
        consumed_at_ms: row
            .try_get("consumed_at_ms")
            .map_err(|error| auth_sql_error("decode auth flow consumed_at_ms", error))?,
        completed_at_ms: row
            .try_get("completed_at_ms")
            .map_err(|error| auth_sql_error("decode auth flow completed_at_ms", error))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| auth_sql_error("decode auth flow created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| auth_sql_error("decode auth flow updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}
