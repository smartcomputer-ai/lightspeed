//! GitHub App driver (P69 G5): app JWT signing and installation access
//! token minting.
//!
//! Unlike OAuth there is no flow and no stored access token: the app's
//! private key (in the secret store) signs a short-lived RS256 JWT, which is
//! exchanged at GitHub's API for a ~1 hour installation token, minted on
//! demand by the broker. A grant with kind `github_app` represents the
//! installation; its non-secret metadata carries the installation id,
//! account, permissions, and repository selection.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AuthBrokerError, AuthGrantId, AuthGrantRecord, AuthGrantStatus, AuthGrantStore,
    AuthProviderConfig, AuthProviderId, AuthProviderStatus, AuthProviderStore,
    AuthRegistryError, DEFAULT_REFRESH_EXPIRY_MARGIN_MS, GrantTokenSource, SecretStore,
    SecretValue,
};

pub const SECRET_KIND_GITHUB_APP_PRIVATE_KEY: &str = "auth.github_app.private_key";

pub const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// App JWTs are valid for at most 10 minutes; sign for 9 with a 60s
/// backdated `iat` to absorb clock skew (per GitHub's own guidance).
const APP_JWT_BACKDATE_SECS: i64 = 60;
const APP_JWT_TTL_SECS: i64 = 9 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GitHubAppError {
    #[error("github app private key is invalid: {message}")]
    InvalidPrivateKey { message: String },

    #[error("github rejected the app credentials: {message}")]
    CredentialsRejected { message: String },

    #[error("github app installation {installation_id} was not found (uninstalled?)")]
    InstallationNotFound { installation_id: i64 },

    #[error("github api request failed{}: {message}", .status.map(|status| format!(" with status {status}")).unwrap_or_default())]
    Http {
        status: Option<u16>,
        message: String,
    },

    #[error("github api returned an invalid response: {message}")]
    InvalidResponse { message: String },

    #[error(transparent)]
    Registry(AuthRegistryError),
}

/// A minted installation access token. `expires_at_ms` comes from GitHub
/// (~1 hour); the broker caches in memory only, never durably.
#[derive(Clone, Debug)]
pub struct GitHubInstallationToken {
    pub token: SecretValue,
    pub expires_at_ms: i64,
}

/// An installation of the app, as listed by GitHub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubInstallation {
    pub installation_id: i64,
    pub account_login: Option<String>,
    pub repository_selection: Option<String>,
    /// Fine-grained permission map as GitHub reports it, e.g.
    /// `{"contents": "read", "metadata": "read"}`.
    pub permissions: serde_json::Value,
}

/// GitHub REST surface the driver needs. Mocked in tests; the real
/// implementation is [`HttpGitHubApiClient`].
#[async_trait]
pub trait GitHubApiClient: Send + Sync {
    async fn list_installations(
        &self,
        api_base_url: &str,
        app_jwt: &SecretValue,
    ) -> Result<Vec<GitHubInstallation>, GitHubAppError>;

    async fn create_installation_token(
        &self,
        api_base_url: &str,
        app_jwt: &SecretValue,
        installation_id: i64,
    ) -> Result<GitHubInstallationToken, GitHubAppError>;
}

#[derive(Serialize)]
struct AppJwtClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

/// Validate a PEM-encoded RSA private key without signing anything; used at
/// import time so a bad key fails at registration, not at mint time.
pub fn validate_github_app_private_key(private_key_pem: &SecretValue) -> Result<(), GitHubAppError> {
    jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.expose().as_bytes())
        .map(|_| ())
        .map_err(|error| GitHubAppError::InvalidPrivateKey {
            message: error.to_string(),
        })
}

/// Sign the app JWT GitHub exchanges for installation tokens (RS256,
/// `iss` = app id).
pub fn sign_github_app_jwt(
    app_id: &str,
    private_key_pem: &SecretValue,
    now_ms: i64,
) -> Result<SecretValue, GitHubAppError> {
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.expose().as_bytes())
        .map_err(|error| GitHubAppError::InvalidPrivateKey {
            message: error.to_string(),
        })?;
    let now_secs = now_ms / 1000;
    let claims = AppJwtClaims {
        iat: now_secs - APP_JWT_BACKDATE_SECS,
        exp: now_secs + APP_JWT_TTL_SECS,
        iss: app_id.to_owned(),
    };
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    jsonwebtoken::encode(&header, &claims, &key)
        .map(SecretValue::new)
        .map_err(|error| GitHubAppError::InvalidPrivateKey {
            message: format!("sign app jwt: {error}"),
        })
}

/// Non-secret metadata stored on a GitHub App installation grant
/// (`AuthGrantRecord.metadata`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GitHubInstallationGrantMetadata {
    pub installation_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_selection: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub permissions: serde_json::Value,
}

impl GitHubInstallationGrantMetadata {
    pub fn from_installation(installation: &GitHubInstallation) -> Self {
        Self {
            installation_id: installation.installation_id,
            account_login: installation.account_login.clone(),
            repository_selection: installation.repository_selection.clone(),
            permissions: installation.permissions.clone(),
        }
    }

    pub fn to_json(&self) -> Result<serde_json::Value, AuthRegistryError> {
        serde_json::to_value(self).map_err(|error| AuthRegistryError::Store {
            message: format!("encode github installation metadata: {error}"),
        })
    }

    /// Parse the typed metadata off a grant. The installation id is
    /// load-bearing (the broker mints against it), so this is validated at
    /// grant creation and re-validated here.
    pub fn from_grant(grant: &AuthGrantRecord) -> Result<Self, AuthRegistryError> {
        let metadata: Self =
            serde_json::from_value(grant.metadata.clone()).map_err(|error| {
                AuthRegistryError::InvalidInput {
                    message: format!(
                        "grant {} has invalid github installation metadata: {error}",
                        grant.grant_id
                    ),
                }
            })?;
        if metadata.installation_id <= 0 {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "grant {} has invalid github installation id {}",
                    grant.grant_id, metadata.installation_id
                ),
            });
        }
        Ok(metadata)
    }
}

/// GitHub App token source for the broker. Installation tokens are never
/// stored durably: each is minted on demand (app JWT -> installation token)
/// and cached only in process memory until its expiry margin. Register under
/// [`crate::AuthProviderKind::GitHubApp`] via
/// `RegistryTokenBroker::with_token_source`; the broker holds the per-grant
/// lock around minting.
#[derive(Clone)]
pub struct GitHubAppRuntime {
    providers: Arc<dyn AuthProviderStore>,
    api: Arc<dyn GitHubApiClient>,
    grants: Arc<dyn AuthGrantStore>,
    secrets: Arc<dyn SecretStore>,
    expiry_margin_ms: i64,
    cache: Arc<Mutex<BTreeMap<AuthGrantId, GitHubInstallationToken>>>,
}

impl GitHubAppRuntime {
    pub fn new(
        providers: Arc<dyn AuthProviderStore>,
        api: Arc<dyn GitHubApiClient>,
        grants: Arc<dyn AuthGrantStore>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            providers,
            api,
            grants,
            secrets,
            expiry_margin_ms: DEFAULT_REFRESH_EXPIRY_MARGIN_MS,
            cache: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn with_expiry_margin_ms(mut self, expiry_margin_ms: i64) -> Self {
        self.expiry_margin_ms = expiry_margin_ms.max(0);
        self
    }

    fn cached_token(&self, grant_id: &AuthGrantId, now_ms: i64) -> Option<SecretValue> {
        let cache = self.cache.lock().ok()?;
        cache.get(grant_id).and_then(|token| {
            (now_ms < token.expires_at_ms.saturating_sub(self.expiry_margin_ms))
                .then(|| token.token.clone())
        })
    }

    fn store_token(&self, grant_id: &AuthGrantId, token: GitHubInstallationToken) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(grant_id.clone(), token);
        }
    }

    /// Mint an installation token for the grant. The broker holds the
    /// per-grant lock and has re-checked the cache.
    async fn mint(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<SecretValue, AuthBrokerError> {
        let grant_id = grant.grant_id.clone();
        let mint_error = |message: String| AuthBrokerError::MintFailed {
            grant_id: grant_id.clone(),
            message,
        };

        let metadata = GitHubInstallationGrantMetadata::from_grant(grant)
            .map_err(|error| mint_error(error.to_string()))?;
        let provider_id = AuthProviderId::try_new(grant.provider_id.clone())
            .map_err(|error| mint_error(format!("invalid provider id: {error}")))?;
        let provider = self
            .providers
            .read_auth_provider(&provider_id)
            .await
            .map_err(|error| mint_error(format!("load auth provider: {error}")))?;
        if provider.status != AuthProviderStatus::Active {
            return Err(mint_error(format!(
                "auth provider {provider_id} is not active: {:?}",
                provider.status
            )));
        }
        let config = match &provider.config {
            AuthProviderConfig::GitHubApp(config) => config,
            #[allow(unreachable_patterns)]
            _ => {
                return Err(mint_error(format!(
                    "auth provider {provider_id} is not a github app"
                )));
            }
        };
        let Some(key_secret) = &provider.credential_secret else {
            return Err(mint_error(format!(
                "auth provider {provider_id} has no private key credential"
            )));
        };
        let (_, private_key) = self
            .secrets
            .read_secret(key_secret)
            .await
            .map_err(|error| AuthBrokerError::Store {
                message: format!("read github app private key: {error}"),
            })?;
        let app_jwt = sign_github_app_jwt(&config.app_id, &private_key, now_ms)
            .map_err(|error| mint_error(error.to_string()))?;

        match self
            .api
            .create_installation_token(&config.api_base_url, &app_jwt, metadata.installation_id)
            .await
        {
            Ok(token) => {
                let value = token.token.clone();
                self.store_token(&grant_id, token);
                Ok(value)
            }
            Err(GitHubAppError::CredentialsRejected { .. }) => {
                // The app key was revoked or rotated; minting cannot succeed
                // until the provider credential is fixed.
                let _ = self
                    .grants
                    .update_grant_status(&grant_id, AuthGrantStatus::Failed, now_ms)
                    .await;
                Err(AuthBrokerError::GrantNotActive {
                    grant_id,
                    status: AuthGrantStatus::Failed,
                })
            }
            Err(GitHubAppError::InstallationNotFound { .. }) => {
                // The app was uninstalled; reinstalling produces a new grant.
                let _ = self
                    .grants
                    .update_grant_status(&grant_id, AuthGrantStatus::NeedsReauth, now_ms)
                    .await;
                Err(AuthBrokerError::GrantNotActive {
                    grant_id,
                    status: AuthGrantStatus::NeedsReauth,
                })
            }
            Err(other) => Err(mint_error(other.to_string())),
        }
    }
}

#[async_trait]
impl GrantTokenSource for GitHubAppRuntime {
    async fn current_token(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<Option<SecretValue>, AuthBrokerError> {
        Ok(self.cached_token(&grant.grant_id, now_ms))
    }

    async fn renew_token(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<SecretValue, AuthBrokerError> {
        self.mint(grant, now_ms).await
    }
}

/// Parse GitHub's RFC 3339 UTC timestamps (`2016-07-11T22:14:10Z`) to epoch
/// milliseconds. GitHub only emits the `Z` offset; anything else is an
/// error rather than a silent guess.
pub(crate) fn parse_rfc3339_utc_ms(value: &str) -> Result<i64, GitHubAppError> {
    let invalid = || GitHubAppError::InvalidResponse {
        message: format!("invalid timestamp {value:?}"),
    };
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return Err(invalid());
    }
    if !value.ends_with('Z') {
        return Err(invalid());
    }
    let digits = |range: std::ops::Range<usize>| -> Result<i64, GitHubAppError> {
        value
            .get(range)
            .and_then(|part| part.parse::<i64>().ok())
            .ok_or_else(invalid)
    };
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    let hour = digits(11..13)?;
    let minute = digits(14..16)?;
    let second = digits(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return Err(invalid());
    }
    // Howard Hinnant's days-from-civil algorithm.
    let years = if month <= 2 { year - 1 } else { year };
    let era = years.div_euclid(400);
    let year_of_era = years - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    Ok(((days_since_epoch * 24 + hour) * 60 + minute) * 60_000 + second * 1000)
}

/// Real GitHub REST client. Sends the app JWT as a bearer header; tokens in
/// responses move straight into [`SecretValue`] wrappers.
pub struct HttpGitHubApiClient {
    http: reqwest::Client,
}

impl HttpGitHubApiClient {
    pub fn new() -> Result<Self, GitHubAppError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("lightspeed-auth-registry")
            .build()
            .map_err(|error| GitHubAppError::Http {
                status: None,
                message: format!("build http client: {error}"),
            })?;
        Ok(Self { http })
    }

    async fn request_json(
        &self,
        method: reqwest::Method,
        url: &str,
        app_jwt: &SecretValue,
        installation_id: Option<i64>,
    ) -> Result<serde_json::Value, GitHubAppError> {
        let response = self
            .http
            .request(method, url)
            .bearer_auth(app_jwt.expose())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| GitHubAppError::Http {
                status: error.status().map(|status| status.as_u16()),
                message: format!("github api request failed: {error}"),
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| GitHubAppError::Http {
            status: Some(status.as_u16()),
            message: format!("read github api response: {error}"),
        })?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(GitHubAppError::CredentialsRejected {
                message: github_error_message(&body),
            });
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            if let Some(installation_id) = installation_id {
                return Err(GitHubAppError::InstallationNotFound { installation_id });
            }
            return Err(GitHubAppError::Http {
                status: Some(404),
                message: github_error_message(&body),
            });
        }
        if !status.is_success() {
            return Err(GitHubAppError::Http {
                status: Some(status.as_u16()),
                message: github_error_message(&body),
            });
        }
        serde_json::from_str(&body).map_err(|_| GitHubAppError::InvalidResponse {
            message: "github api response is not valid JSON".to_owned(),
        })
    }
}

/// GitHub error bodies are `{"message": "...", ...}`; surface only that
/// field, never raw bodies.
fn github_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(|message| message.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "github api returned a non-JSON error body".to_owned())
}

fn installation_from_json(value: &serde_json::Value) -> Result<GitHubInstallation, GitHubAppError> {
    let Some(installation_id) = value.get("id").and_then(|id| id.as_i64()) else {
        return Err(GitHubAppError::InvalidResponse {
            message: "installation entry is missing id".to_owned(),
        });
    };
    Ok(GitHubInstallation {
        installation_id,
        account_login: value
            .pointer("/account/login")
            .and_then(|login| login.as_str())
            .map(str::to_owned),
        repository_selection: value
            .get("repository_selection")
            .and_then(|selection| selection.as_str())
            .map(str::to_owned),
        permissions: value
            .get("permissions")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    })
}

#[async_trait]
impl GitHubApiClient for HttpGitHubApiClient {
    async fn list_installations(
        &self,
        api_base_url: &str,
        app_jwt: &SecretValue,
    ) -> Result<Vec<GitHubInstallation>, GitHubAppError> {
        let url = format!(
            "{}/app/installations?per_page=100",
            api_base_url.trim_end_matches('/')
        );
        let value = self
            .request_json(reqwest::Method::GET, &url, app_jwt, None)
            .await?;
        let Some(entries) = value.as_array() else {
            return Err(GitHubAppError::InvalidResponse {
                message: "installations response is not an array".to_owned(),
            });
        };
        entries.iter().map(installation_from_json).collect()
    }

    async fn create_installation_token(
        &self,
        api_base_url: &str,
        app_jwt: &SecretValue,
        installation_id: i64,
    ) -> Result<GitHubInstallationToken, GitHubAppError> {
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            api_base_url.trim_end_matches('/')
        );
        let value = self
            .request_json(reqwest::Method::POST, &url, app_jwt, Some(installation_id))
            .await?;
        let Some(token) = value.get("token").and_then(|token| token.as_str()) else {
            return Err(GitHubAppError::InvalidResponse {
                message: "token response is missing token".to_owned(),
            });
        };
        let Some(expires_at) = value.get("expires_at").and_then(|expires| expires.as_str())
        else {
            return Err(GitHubAppError::InvalidResponse {
                message: "token response is missing expires_at".to_owned(),
            });
        };
        Ok(GitHubInstallationToken {
            token: SecretValue::new(token),
            expires_at_ms: parse_rfc3339_utc_ms(expires_at)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2048-bit RSA test key, generated for these tests only; not a real
    // credential anywhere.
    const TEST_RSA_KEY: &str = include_str!("../testdata/github_app_test_key.pem");

    #[test]
    fn rfc3339_utc_timestamps_parse_to_epoch_ms() {
        assert_eq!(
            parse_rfc3339_utc_ms("1970-01-01T00:00:00Z").expect("epoch"),
            0
        );
        assert_eq!(
            parse_rfc3339_utc_ms("2016-07-11T22:14:10Z").expect("github docs example"),
            1_468_275_250_000
        );
        assert!(parse_rfc3339_utc_ms("2016-07-11T22:14:10+02:00").is_err());
        assert!(parse_rfc3339_utc_ms("not a date").is_err());
    }

    #[test]
    fn app_jwts_sign_and_verify_with_expected_claims() {
        let key = SecretValue::new(TEST_RSA_KEY);
        validate_github_app_private_key(&key).expect("test key parses");

        let now_ms = 1_700_000_000_000;
        let jwt = sign_github_app_jwt("12345", &key, now_ms).expect("sign jwt");

        let decoding_key = jsonwebtoken::DecodingKey::from_rsa_pem(
            include_str!("../testdata/github_app_test_key_pub.pem").as_bytes(),
        )
        .expect("public test key");
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.validate_exp = false;
        validation.set_required_spec_claims::<&str>(&[]);
        let decoded = jsonwebtoken::decode::<serde_json::Value>(
            jwt.expose(),
            &decoding_key,
            &validation,
        )
        .expect("verify jwt");
        assert_eq!(decoded.claims["iss"], "12345");
        assert_eq!(decoded.claims["iat"], 1_700_000_000 - 60);
        assert_eq!(decoded.claims["exp"], 1_700_000_000 + 540);
    }

    #[test]
    fn invalid_private_keys_are_rejected_at_validation() {
        let error = validate_github_app_private_key(&SecretValue::new("not a pem"))
            .expect_err("garbage key must fail");

        assert!(matches!(error, GitHubAppError::InvalidPrivateKey { .. }));
    }

    #[test]
    fn installation_metadata_round_trips_through_grant_json() {
        let installation = GitHubInstallation {
            installation_id: 678,
            account_login: Some("acme".to_owned()),
            repository_selection: Some("selected".to_owned()),
            permissions: serde_json::json!({"contents": "read"}),
        };

        let metadata = GitHubInstallationGrantMetadata::from_installation(&installation);
        let json = metadata.to_json().expect("encode metadata");

        assert_eq!(json["installation_id"], 678);
        assert_eq!(json["account_login"], "acme");
        let decoded: GitHubInstallationGrantMetadata =
            serde_json::from_value(json).expect("decode metadata");
        assert_eq!(decoded, metadata);
    }

    use std::sync::atomic::{AtomicI64, Ordering};

    use crate::{
        AuthProviderKind, AuthTokenBroker, CreateAuthGrantRecord, CreateAuthProviderRecord,
        GitHubAppConfig, InMemoryAuthGrantStore, InMemoryAuthProviderStore, InMemoryGrantLocks,
        InMemorySecretStore, PrincipalRef, PutSecretRecord, RegistryTokenBroker, SecretId,
        TokenAudience,
    };

    struct CountingGitHubApi {
        responses: Mutex<Vec<Result<GitHubInstallationToken, GitHubAppError>>>,
        calls: AtomicI64,
    }

    #[async_trait]
    impl GitHubApiClient for CountingGitHubApi {
        async fn list_installations(
            &self,
            _api_base_url: &str,
            _app_jwt: &SecretValue,
        ) -> Result<Vec<GitHubInstallation>, GitHubAppError> {
            Ok(Vec::new())
        }

        async fn create_installation_token(
            &self,
            _api_base_url: &str,
            app_jwt: &SecretValue,
            installation_id: i64,
        ) -> Result<GitHubInstallationToken, GitHubAppError> {
            assert!(!app_jwt.expose().is_empty(), "app jwt must be signed");
            assert_eq!(installation_id, 678);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses.lock().expect("lock").remove(0)
        }
    }

    struct GitHubHarness {
        broker: RegistryTokenBroker,
        grants: Arc<InMemoryAuthGrantStore>,
        api: Arc<CountingGitHubApi>,
        now: Arc<AtomicI64>,
    }

    async fn github_harness(
        responses: Vec<Result<GitHubInstallationToken, GitHubAppError>>,
    ) -> GitHubHarness {
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let providers = Arc::new(InMemoryAuthProviderStore::new());
        secrets
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_appkey"),
                secret_kind: SECRET_KIND_GITHUB_APP_PRIVATE_KEY.to_owned(),
                value: SecretValue::new(TEST_RSA_KEY),
                created_at_ms: 10,
            })
            .await
            .expect("put app key");
        providers
            .create_auth_provider(CreateAuthProviderRecord {
                provider_id: AuthProviderId::new("lightspeed-github"),
                display_name: None,
                config: AuthProviderConfig::GitHubApp(GitHubAppConfig {
                    app_id: "12345".to_owned(),
                    api_base_url: "https://api.github.com".to_owned(),
                }),
                credential_secret: Some(SecretId::new("authsec_appkey")),
                status: AuthProviderStatus::Active,
                created_at_ms: 10,
            })
            .await
            .expect("create provider");
        grants
            .create_grant(CreateAuthGrantRecord {
                grant_id: AuthGrantId::new("authgrant_install"),
                provider_id: "lightspeed-github".to_owned(),
                provider_kind: AuthProviderKind::GitHubApp,
                principal: PrincipalRef::universe_default(),
                display_name: None,
                subject_hint: Some("acme".to_owned()),
                scopes: Vec::new(),
                audience: Some("https://api.github.com".to_owned()),
                access_token_secret: None,
                refresh_token_secret: None,
                oauth_client: None,
                expires_at_ms: None,
                status: AuthGrantStatus::Active,
                metadata: serde_json::json!({
                    "installation_id": 678,
                    "account_login": "acme",
                }),
                created_at_ms: 10,
            })
            .await
            .expect("create installation grant");
        let api = Arc::new(CountingGitHubApi {
            responses: Mutex::new(responses),
            calls: AtomicI64::new(0),
        });
        let now = Arc::new(AtomicI64::new(1_000_000));
        let now_for_fn = now.clone();
        let broker = RegistryTokenBroker::new(
            grants.clone(),
            secrets.clone(),
            Arc::new(InMemoryGrantLocks::new()),
        )
        .with_token_source(
            AuthProviderKind::GitHubApp,
            Arc::new(
                GitHubAppRuntime::new(providers, api.clone(), grants.clone(), secrets)
                    .with_expiry_margin_ms(100),
            ),
        )
        .with_now_fn(Arc::new(move || now_for_fn.load(Ordering::SeqCst)));
        GitHubHarness {
            broker,
            grants,
            api,
            now,
        }
    }

    fn minted(token: &str, expires_at_ms: i64) -> GitHubInstallationToken {
        GitHubInstallationToken {
            token: SecretValue::new(token),
            expires_at_ms,
        }
    }

    fn install_grant_id() -> AuthGrantId {
        AuthGrantId::new("authgrant_install")
    }

    fn github_audience() -> TokenAudience {
        TokenAudience::GitHubApi("https://api.github.com".to_owned())
    }

    #[tokio::test]
    async fn github_grants_mint_installation_tokens_and_cache_them() {
        let harness = github_harness(vec![
            Ok(minted("ghs_one", 1_000_000 + 3_600_000)),
            Ok(minted("ghs_two", 1_000_000 + 7_200_000)),
        ])
        .await;

        let first = harness
            .broker
            .bearer_token(&install_grant_id(), &github_audience())
            .await
            .expect("mint token");
        let second = harness
            .broker
            .bearer_token(&install_grant_id(), &github_audience())
            .await
            .expect("cached token");

        assert_eq!(first.expose(), "ghs_one");
        assert_eq!(second.expose(), "ghs_one");
        assert_eq!(harness.api.calls.load(Ordering::SeqCst), 1);

        // Past the cached token's expiry a new one is minted.
        harness.now.store(1_000_000 + 3_600_000, Ordering::SeqCst);
        let third = harness
            .broker
            .bearer_token(&install_grant_id(), &github_audience())
            .await
            .expect("re-mint token");
        assert_eq!(third.expose(), "ghs_two");
        assert_eq!(harness.api.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn github_grants_enforce_audience() {
        let harness = github_harness(Vec::new()).await;

        let error = harness
            .broker
            .bearer_token(
                &install_grant_id(),
                &TokenAudience::GitHubApi("https://api.evil.example.com".to_owned()),
            )
            .await
            .expect_err("audience mismatch must fail");

        assert!(matches!(error, AuthBrokerError::AudienceMismatch { .. }));
    }

    #[tokio::test]
    async fn rejected_app_credentials_mark_the_grant_failed() {
        let harness = github_harness(vec![Err(GitHubAppError::CredentialsRejected {
            message: "bad credentials".to_owned(),
        })])
        .await;

        let error = harness
            .broker
            .bearer_token(&install_grant_id(), &github_audience())
            .await
            .expect_err("rejected credentials must fail");

        assert!(matches!(
            error,
            AuthBrokerError::GrantNotActive {
                status: AuthGrantStatus::Failed,
                ..
            }
        ));
        let grant = harness
            .grants
            .read_grant(&install_grant_id())
            .await
            .expect("grant");
        assert_eq!(grant.status, AuthGrantStatus::Failed);
    }

    #[tokio::test]
    async fn uninstalled_apps_mark_the_grant_needs_reauth() {
        let harness = github_harness(vec![Err(GitHubAppError::InstallationNotFound {
            installation_id: 678,
        })])
        .await;

        let error = harness
            .broker
            .bearer_token(&install_grant_id(), &github_audience())
            .await
            .expect_err("uninstalled app must fail");

        assert!(matches!(
            error,
            AuthBrokerError::GrantNotActive {
                status: AuthGrantStatus::NeedsReauth,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn transient_github_failures_do_not_poison_the_grant() {
        let harness = github_harness(vec![Err(GitHubAppError::Http {
            status: Some(503),
            message: "unavailable".to_owned(),
        })])
        .await;

        let error = harness
            .broker
            .bearer_token(&install_grant_id(), &github_audience())
            .await
            .expect_err("transient failure surfaces");

        assert!(matches!(error, AuthBrokerError::MintFailed { .. }));
        let grant = harness
            .grants
            .read_grant(&install_grant_id())
            .await
            .expect("grant");
        assert_eq!(grant.status, AuthGrantStatus::Active);
    }
}
