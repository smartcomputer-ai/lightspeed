//! Microsoft Entra workload identity authentication for PostgreSQL.
//!
//! The server accepts an Entra access token as the login password. It is checked
//! when a connection opens, so only new connections need a current token. sqlx has
//! no per-connection credential callback, so a background task keeps the pool's
//! connect options current. Refreshing at half of each token's lifetime leaves the
//! other half to ride out an Entra outage before new connections start failing.
use std::{str::FromStr, sync::Arc, time::Duration};

use object_store::azure::authority_hosts::AZURE_PUBLIC_CLOUD;
use serde::Deserialize;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use thiserror::Error;
use url::Url;

use super::workload_identity::{
    AzureWorkloadIdentity, AzureWorkloadIdentityError, TokenAcquisitionError, diagnostic_summary,
    render_chain,
};

const POSTGRES_SCOPE: &str = "https://ossrdbms-aad.database.windows.net/.default";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The shortest wait between exchanges, and the wait after a failed one.
const MIN_REFRESH_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum AzurePostgresError {
    #[error(transparent)]
    Identity(#[from] AzureWorkloadIdentityError),
    #[error("the PostgreSQL URL could not be parsed")]
    InvalidUrl,
    #[error("the PostgreSQL URL must name the managed identity as its user")]
    MissingUser,
    #[error("the PostgreSQL URL must not contain a password with workload identity")]
    PasswordInUrl,
    /// The token travels as a cleartext password inside TLS, so the server's
    /// identity must be verified before it is sent.
    #[error("the PostgreSQL URL must set sslmode=verify-full with workload identity")]
    TlsNotVerified,
    #[error(transparent)]
    Token(#[from] TokenAcquisitionError),
    #[error("could not connect to PostgreSQL with the Entra token: {0}")]
    Connect(#[source] sqlx::Error),
}

/// Open a pool that authenticates as the workload's federated identity and renews
/// its token until the pool is closed. Holding a clone of the pool keeps that task
/// and the connections alive, so a pool that should end must be closed explicitly.
pub async fn connect_postgres_with_workload_identity(
    database_url: &str,
    identity: AzureWorkloadIdentity,
) -> Result<PgPool, AzurePostgresError> {
    let options = connect_options(database_url)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        // A redirect could carry the assertion to another host.
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| TokenAcquisitionError::Exchange {
            detail: render_chain(&error),
        })?;
    let source = TokenSource::new(client, identity)?;
    connect(
        options,
        crate::postgres_pool_options(),
        source,
        MIN_REFRESH_DELAY,
    )
    .await
}

fn connect_options(database_url: &str) -> Result<PgConnectOptions, AzurePostgresError> {
    let url = Url::parse(database_url).map_err(|_| AzurePostgresError::InvalidUrl)?;
    if url.username().is_empty() {
        return Err(AzurePostgresError::MissingUser);
    }
    if url.password().is_some() || url.query_pairs().any(|(key, _)| key == "password") {
        return Err(AzurePostgresError::PasswordInUrl);
    }
    let options =
        PgConnectOptions::from_str(database_url).map_err(|_| AzurePostgresError::InvalidUrl)?;
    if !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull) {
        return Err(AzurePostgresError::TlsNotVerified);
    }
    Ok(options)
}

async fn connect(
    options: PgConnectOptions,
    pool_options: PgPoolOptions,
    source: TokenSource,
    min_refresh_delay: Duration,
) -> Result<PgPool, AzurePostgresError> {
    let token = source.fetch().await?;
    let pool = pool_options
        .connect_with(options.clone().password(&token.value))
        .await
        .map_err(AzurePostgresError::Connect)?;
    tokio::spawn(keep_token_current(
        pool.clone(),
        options,
        source,
        refresh_delay(token.expires_in, min_refresh_delay),
        min_refresh_delay,
    ));
    Ok(pool)
}

async fn keep_token_current(
    pool: PgPool,
    options: PgConnectOptions,
    source: TokenSource,
    mut delay: Duration,
    min_delay: Duration,
) {
    loop {
        tokio::select! {
            () = pool.close_event() => return,
            () = tokio::time::sleep(delay) => {}
        }
        delay = match source.fetch().await {
            Ok(token) => {
                pool.set_connect_options(options.clone().password(&token.value));
                refresh_delay(token.expires_in, min_delay)
            }
            Err(error) => {
                tracing::warn!(%error, "could not renew the PostgreSQL access token; open connections are unaffected");
                min_delay
            }
        };
    }
}

fn refresh_delay(expires_in: Duration, min_delay: Duration) -> Duration {
    (expires_in / 2).max(min_delay)
}

/// Exchanges the projected token for PostgreSQL access tokens. The library's
/// provider used for Blob storage has its scope fixed to Azure Storage.
struct TokenSource {
    client: reqwest::Client,
    token_url: Arc<str>,
    client_id: String,
    token_file: String,
}

struct AccessToken {
    value: String,
    expires_in: Duration,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl TokenSource {
    fn new(
        client: reqwest::Client,
        identity: AzureWorkloadIdentity,
    ) -> Result<Self, AzureWorkloadIdentityError> {
        identity.validate()?;
        let authority = identity
            .normalized_authority_host()?
            .unwrap_or_else(|| AZURE_PUBLIC_CLOUD.to_owned());
        Ok(Self {
            client,
            token_url: format!("{authority}/{}/oauth2/v2.0/token", identity.tenant_id).into(),
            client_id: identity.client_id,
            token_file: identity.federated_token_file,
        })
    }

    async fn fetch(&self) -> Result<AccessToken, TokenAcquisitionError> {
        // Read for every exchange: the kubelet replaces this file as it rotates.
        let assertion = tokio::fs::read_to_string(&self.token_file)
            .await
            .map_err(|error| TokenAcquisitionError::TokenFile {
                path: self.token_file.clone(),
                reason: Some(error.kind()),
            })?;
        let response = self
            .client
            .post(&*self.token_url)
            .form(&[
                ("client_id", self.client_id.as_str()),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", assertion.as_str()),
                ("scope", POSTGRES_SCOPE),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|error| TokenAcquisitionError::Exchange {
                detail: render_chain(&error),
            })?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .map_err(|_| TokenAcquisitionError::InvalidResponse)?;
        if !status.is_success() {
            return Err(TokenAcquisitionError::Exchange {
                detail: format!("HTTP {status}: {}", diagnostic_summary(&headers, &body)),
            });
        }
        let token: TokenResponse =
            serde_json::from_slice(&body).map_err(|_| TokenAcquisitionError::InvalidResponse)?;
        Ok(AccessToken {
            value: token.access_token,
            expires_in: Duration::from_secs(token.expires_in),
        })
    }
}

#[cfg(test)]
mod tests;
