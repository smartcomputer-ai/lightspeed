use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use thiserror::Error;

use crate::{
    AuthGrantId, AuthGrantRecord, AuthGrantStatus, AuthGrantStore, AuthGrantTokenRefresh,
    AuthProviderKind, AuthRegistryError, GrantRefreshLock, OAuthClientStore, OAuthTokenClient,
    OAuthTokenError, OAuthTokenGrant, OAuthTokenRequest, PutSecretRecord,
    SECRET_KIND_OAUTH_ACCESS_TOKEN, SECRET_KIND_OAUTH_REFRESH_TOKEN, SecretId, SecretStore,
    SecretValue, random_auth_id,
};

/// The resource a resolved token is about to be sent to. Audience enforcement
/// is the broker's job: a grant bound to an audience only resolves for
/// resources that audience covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenAudience {
    /// Canonical remote MCP server resource URL (RFC 8707 resource).
    McpResource(String),
    /// GitHub REST API base URL the installation token is for.
    GitHubApi(String),
}

impl TokenAudience {
    fn resource(&self) -> &str {
        match self {
            Self::McpResource(resource) => resource,
            Self::GitHubApi(resource) => resource,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthBrokerError {
    #[error("auth grant not found: {grant_id}")]
    GrantNotFound { grant_id: AuthGrantId },

    #[error("auth grant {grant_id} is not active: {status:?}")]
    GrantNotActive {
        grant_id: AuthGrantId,
        status: AuthGrantStatus,
    },

    #[error("auth grant {grant_id} is expired")]
    GrantExpired { grant_id: AuthGrantId },

    #[error("auth grant {grant_id} does not cover audience {requested}")]
    AudienceMismatch {
        grant_id: AuthGrantId,
        requested: String,
    },

    #[error("auth grant {grant_id} has no resolvable token secret")]
    SecretMissing { grant_id: AuthGrantId },

    #[error("auth grant {grant_id} token refresh failed: {message}")]
    RefreshFailed {
        grant_id: AuthGrantId,
        message: String,
    },

    #[error("auth grant {grant_id} token mint failed: {message}")]
    MintFailed {
        grant_id: AuthGrantId,
        message: String,
    },

    #[error("no token source is configured for provider kind {provider_kind:?} (grant {grant_id})")]
    SourceNotConfigured {
        grant_id: AuthGrantId,
        provider_kind: AuthProviderKind,
    },

    #[error("auth broker store failure: {message}")]
    Store { message: String },
}

#[async_trait]
pub trait AuthTokenBroker: Send + Sync {
    /// Resolve a current bearer token for `grant_id`, enforcing grant status,
    /// expiry, and audience, renewing (refreshing/minting) when needed. Fails
    /// with a typed error; never returns a silent absence. Optional-auth-absent
    /// is a link-policy concern expressed by omitting `auth_ref` upstream, not
    /// by the broker.
    async fn bearer_token(
        &self,
        grant_id: &AuthGrantId,
        audience: &TokenAudience,
    ) -> Result<SecretValue, AuthBrokerError>;
}

/// Provider-kind-specific token resolution behind the generic broker. The
/// broker owns grant loading, status/audience enforcement, and per-grant
/// single-flight locking; sources own how a token is obtained for their
/// provider kind (stored secret, OAuth refresh, on-demand mint, ...).
#[async_trait]
pub trait GrantTokenSource: Send + Sync {
    /// Fast path, called without the per-grant lock: return a token that is
    /// currently valid (outside any expiry margin), `None` when renewal is
    /// needed, or a typed error when the grant cannot resolve at all.
    async fn current_token(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<Option<SecretValue>, AuthBrokerError>;

    /// Slow path, called with the per-grant lock held after the grant was
    /// re-read and `current_token` re-checked. Implementations own their
    /// provider-specific grant-status transitions (`NeedsReauth`, `Failed`).
    async fn renew_token(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<SecretValue, AuthBrokerError>;
}

/// Returns true when `audience` covers `resource`: exact match, or a
/// path-boundary prefix match. Comparison is byte-wise; audiences and
/// resources are validated, normalized URLs.
pub fn audience_covers(audience: &str, resource: &str) -> bool {
    if audience == resource {
        return true;
    }
    if let Some(rest) = resource.strip_prefix(audience) {
        return audience.ends_with('/') || rest.starts_with('/') || rest.starts_with('?');
    }
    false
}

/// Renew tokens this close to (or past) `expires_at_ms` instead of using
/// the stored or cached token, so a token does not expire mid provider call.
pub const DEFAULT_REFRESH_EXPIRY_MARGIN_MS: i64 = 60_000;

/// OAuth refresh dependencies for [`RegistryTokenBroker`]. Without this
/// configuration the broker resolves stored tokens only and OAuth grants
/// expire instead of refreshing.
#[derive(Clone)]
pub struct OAuthRefreshRuntime {
    pub clients: Arc<dyn OAuthClientStore>,
    pub token_client: Arc<dyn OAuthTokenClient>,
    pub expiry_margin_ms: i64,
}

impl OAuthRefreshRuntime {
    pub fn new(clients: Arc<dyn OAuthClientStore>, token_client: Arc<dyn OAuthTokenClient>) -> Self {
        Self {
            clients,
            token_client,
            expiry_margin_ms: DEFAULT_REFRESH_EXPIRY_MARGIN_MS,
        }
    }

    pub fn with_expiry_margin_ms(mut self, expiry_margin_ms: i64) -> Self {
        self.expiry_margin_ms = expiry_margin_ms.max(0);
        self
    }
}

/// Provider kinds whose grants carry a stored access token (optionally
/// refreshable through OAuth). Served by [`StoredTokenSource`].
const STORED_TOKEN_KINDS: [AuthProviderKind; 5] = [
    AuthProviderKind::StaticBearer,
    AuthProviderKind::McpOAuth,
    AuthProviderKind::CustomOAuth,
    AuthProviderKind::GitHubAppUser,
    AuthProviderKind::GitHubOAuthApp,
];

/// Token source for grants with stored access tokens: static bearer imports
/// and OAuth grants. With an [`OAuthRefreshRuntime`] configured, expiring
/// OAuth tokens refresh (with rotation persisted atomically); without one,
/// stored tokens are served until they expire.
struct StoredTokenSource {
    grants: Arc<dyn AuthGrantStore>,
    secrets: Arc<dyn SecretStore>,
    refresh: Option<OAuthRefreshRuntime>,
}

impl StoredTokenSource {
    async fn read_access_token(
        &self,
        grant: &AuthGrantRecord,
    ) -> Result<SecretValue, AuthBrokerError> {
        let Some(secret_id) = &grant.access_token_secret else {
            return Err(AuthBrokerError::SecretMissing {
                grant_id: grant.grant_id.clone(),
            });
        };
        let (_, value) = self
            .secrets
            .read_secret(secret_id)
            .await
            .map_err(|error| match error {
                AuthRegistryError::SecretNotFound { .. } => AuthBrokerError::SecretMissing {
                    grant_id: grant.grant_id.clone(),
                },
                other => AuthBrokerError::Store {
                    message: other.to_string(),
                },
            })?;
        Ok(value)
    }

    /// The stored access token can be served as-is: it exists and is not
    /// inside the refresh margin.
    fn token_is_fresh(grant: &AuthGrantRecord, now_ms: i64, margin_ms: i64) -> bool {
        grant.access_token_secret.is_some()
            && grant
                .expires_at_ms
                .is_none_or(|expires_at_ms| now_ms < expires_at_ms.saturating_sub(margin_ms))
    }

    fn token_is_usable(grant: &AuthGrantRecord, now_ms: i64) -> bool {
        grant.access_token_secret.is_some()
            && grant
                .expires_at_ms
                .is_none_or(|expires_at_ms| now_ms < expires_at_ms)
    }

    fn refresh_available(&self, grant: &AuthGrantRecord) -> bool {
        self.refresh.is_some()
            && grant.refresh_token_secret.is_some()
            && grant.oauth_client.is_some()
    }

    /// Refresh the grant's tokens. Caller holds the per-grant lock and has
    /// re-checked freshness. Returns the new access token.
    async fn refresh_grant(
        &self,
        grant: &AuthGrantRecord,
        oauth: &OAuthRefreshRuntime,
        now_ms: i64,
    ) -> Result<SecretValue, AuthBrokerError> {
        let grant_id = grant.grant_id.clone();
        let store_error = |message: String| AuthBrokerError::Store { message };
        let refresh_error = |message: String| AuthBrokerError::RefreshFailed {
            grant_id: grant_id.clone(),
            message,
        };

        let client_id = grant
            .oauth_client
            .as_ref()
            .ok_or_else(|| refresh_error("grant has no oauth client reference".to_owned()))?;
        let client = oauth
            .clients
            .read_oauth_client(client_id)
            .await
            .map_err(|error| refresh_error(format!("load oauth client: {error}")))?;
        let refresh_secret_id = grant
            .refresh_token_secret
            .clone()
            .ok_or_else(|| refresh_error("grant has no refresh token".to_owned()))?;
        let (_, refresh_token) = self
            .secrets
            .read_secret(&refresh_secret_id)
            .await
            .map_err(|error| store_error(format!("read refresh token: {error}")))?;
        let client_secret = match &client.client_secret {
            Some(secret_id) => Some(
                self.secrets
                    .read_secret(secret_id)
                    .await
                    .map_err(|error| store_error(format!("read client secret: {error}")))?
                    .1,
            ),
            None => None,
        };

        let request = OAuthTokenRequest {
            token_endpoint: client.token_endpoint.clone(),
            remote_client_id: client.remote_client_id.clone(),
            client_secret,
            auth_method: client.token_endpoint_auth_method,
            grant: OAuthTokenGrant::RefreshToken { refresh_token },
            resource: grant.audience.clone(),
        };
        let response = match oauth.token_client.request_token(&request).await {
            Ok(response) => response,
            Err(OAuthTokenError::InvalidGrant { .. }) => {
                // The refresh token is dead; further attempts cannot succeed
                // until the user reauthorizes.
                let _ = self
                    .grants
                    .update_grant_status(&grant_id, AuthGrantStatus::NeedsReauth, now_ms)
                    .await;
                return Err(AuthBrokerError::GrantNotActive {
                    grant_id: grant_id.clone(),
                    status: AuthGrantStatus::NeedsReauth,
                });
            }
            Err(error) => return Err(refresh_error(error.to_string())),
        };
        if !response.token_type.eq_ignore_ascii_case("bearer") {
            return Err(refresh_error(format!(
                "unsupported token_type {:?}",
                response.token_type
            )));
        }

        // Persist the rotated refresh token first: once the AS rotated it,
        // losing the new value strands the grant.
        let new_refresh_secret_id = match &response.refresh_token {
            Some(refresh_token) => {
                let secret_id = random_secret_id()?;
                self.secrets
                    .put_secret(PutSecretRecord {
                        secret_id: secret_id.clone(),
                        secret_kind: SECRET_KIND_OAUTH_REFRESH_TOKEN.to_owned(),
                        value: refresh_token.clone(),
                        created_at_ms: now_ms,
                    })
                    .await
                    .map_err(|error| store_error(format!("store rotated refresh token: {error}")))?;
                Some(secret_id)
            }
            None => None,
        };
        let new_access_secret_id = random_secret_id()?;
        self.secrets
            .put_secret(PutSecretRecord {
                secret_id: new_access_secret_id.clone(),
                secret_kind: SECRET_KIND_OAUTH_ACCESS_TOKEN.to_owned(),
                value: response.access_token.clone(),
                created_at_ms: now_ms,
            })
            .await
            .map_err(|error| store_error(format!("store refreshed access token: {error}")))?;

        let old_access_secret = grant.access_token_secret.clone();
        let old_refresh_secret = grant.refresh_token_secret.clone();
        self.grants
            .record_grant_refresh(&grant_id, AuthGrantTokenRefresh {
                access_token_secret: new_access_secret_id,
                refresh_token_secret: new_refresh_secret_id.clone(),
                expires_at_ms: response
                    .expires_in_secs
                    .map(|secs| now_ms.saturating_add(secs.saturating_mul(1000))),
                updated_at_ms: now_ms,
            })
            .await
            .map_err(|error| store_error(format!("record grant refresh: {error}")))?;

        // The grant row now points at the new secrets; the old ones are
        // unreachable and removed best-effort.
        if let Some(old_access_secret) = old_access_secret {
            let _ = self.secrets.delete_secret(&old_access_secret).await;
        }
        if new_refresh_secret_id.is_some() {
            if let Some(old_refresh_secret) = old_refresh_secret {
                let _ = self.secrets.delete_secret(&old_refresh_secret).await;
            }
        }
        Ok(response.access_token)
    }
}

#[async_trait]
impl GrantTokenSource for StoredTokenSource {
    async fn current_token(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<Option<SecretValue>, AuthBrokerError> {
        let margin_ms = self
            .refresh
            .as_ref()
            .map_or(0, |refresh| refresh.expiry_margin_ms);
        if Self::token_is_fresh(grant, now_ms, margin_ms) {
            return self.read_access_token(grant).await.map(Some);
        }
        if !self.refresh_available(grant) {
            if grant
                .expires_at_ms
                .is_some_and(|expires_at_ms| now_ms >= expires_at_ms)
            {
                return Err(AuthBrokerError::GrantExpired {
                    grant_id: grant.grant_id.clone(),
                });
            }
            // Inside the margin without a refresh path: still valid.
            return self.read_access_token(grant).await.map(Some);
        }
        Ok(None)
    }

    async fn renew_token(
        &self,
        grant: &AuthGrantRecord,
        now_ms: i64,
    ) -> Result<SecretValue, AuthBrokerError> {
        let Some(oauth) = &self.refresh else {
            // current_token only defers to renewal when refresh is available.
            return Err(AuthBrokerError::RefreshFailed {
                grant_id: grant.grant_id.clone(),
                message: "oauth refresh is not configured".to_owned(),
            });
        };
        match self.refresh_grant(grant, oauth, now_ms).await {
            Ok(token) => Ok(token),
            Err(error @ AuthBrokerError::GrantNotActive { .. }) => Err(error),
            Err(error) => {
                // Inside the margin but not yet expired the stored token
                // is still valid; serve it instead of failing the call on
                // a transient refresh problem.
                if Self::token_is_usable(grant, now_ms) {
                    return self.read_access_token(grant).await;
                }
                Err(error)
            }
        }
    }
}

/// Broker over the registry store traits. Loads grants, enforces status and
/// audience, and serializes renewal single-flight per grant; how a token is
/// obtained for each provider kind is delegated to the registered
/// [`GrantTokenSource`]s (stored/OAuth-refreshable tokens built in, on-demand
/// minters such as the GitHub App runtime registered via
/// [`RegistryTokenBroker::with_token_source`]).
#[derive(Clone)]
pub struct RegistryTokenBroker {
    grants: Arc<dyn AuthGrantStore>,
    secrets: Arc<dyn SecretStore>,
    locks: Arc<dyn GrantRefreshLock>,
    sources: BTreeMap<AuthProviderKind, Arc<dyn GrantTokenSource>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl RegistryTokenBroker {
    pub fn new(
        grants: Arc<dyn AuthGrantStore>,
        secrets: Arc<dyn SecretStore>,
        locks: Arc<dyn GrantRefreshLock>,
    ) -> Self {
        let mut broker = Self {
            grants,
            secrets,
            locks,
            sources: BTreeMap::new(),
            now_ms: Arc::new(system_now_ms),
        };
        broker.register_stored_source(None);
        broker
    }

    pub fn with_oauth_refresh(mut self, oauth: OAuthRefreshRuntime) -> Self {
        self.register_stored_source(Some(oauth));
        self
    }

    /// Register the token source for a provider kind, replacing any existing
    /// registration for that kind.
    pub fn with_token_source(
        mut self,
        provider_kind: AuthProviderKind,
        source: Arc<dyn GrantTokenSource>,
    ) -> Self {
        self.sources.insert(provider_kind, source);
        self
    }

    pub fn with_now_fn(mut self, now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.now_ms = now_ms;
        self
    }

    fn register_stored_source(&mut self, refresh: Option<OAuthRefreshRuntime>) {
        let source: Arc<dyn GrantTokenSource> = Arc::new(StoredTokenSource {
            grants: self.grants.clone(),
            secrets: self.secrets.clone(),
            refresh,
        });
        for kind in STORED_TOKEN_KINDS {
            self.sources.insert(kind, source.clone());
        }
    }

    async fn read_checked_grant(
        &self,
        grant_id: &AuthGrantId,
        audience: &TokenAudience,
    ) -> Result<AuthGrantRecord, AuthBrokerError> {
        let grant = self
            .grants
            .read_grant(grant_id)
            .await
            .map_err(|error| match error {
                AuthRegistryError::GrantNotFound { grant_id } => {
                    AuthBrokerError::GrantNotFound { grant_id }
                }
                other => AuthBrokerError::Store {
                    message: other.to_string(),
                },
            })?;
        if grant.status != AuthGrantStatus::Active {
            return Err(AuthBrokerError::GrantNotActive {
                grant_id: grant.grant_id,
                status: grant.status,
            });
        }
        if let Some(grant_audience) = &grant.audience {
            if !audience_covers(grant_audience, audience.resource()) {
                return Err(AuthBrokerError::AudienceMismatch {
                    grant_id: grant.grant_id,
                    requested: audience.resource().to_owned(),
                });
            }
        }
        Ok(grant)
    }
}

fn random_secret_id() -> Result<SecretId, AuthBrokerError> {
    SecretId::try_new(random_auth_id("authsec_")).map_err(|error| AuthBrokerError::Store {
        message: format!("generate secret id: {error}"),
    })
}

pub(crate) fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[async_trait]
impl AuthTokenBroker for RegistryTokenBroker {
    async fn bearer_token(
        &self,
        grant_id: &AuthGrantId,
        audience: &TokenAudience,
    ) -> Result<SecretValue, AuthBrokerError> {
        let grant = self.read_checked_grant(grant_id, audience).await?;
        let Some(source) = self.sources.get(&grant.provider_kind).cloned() else {
            return Err(AuthBrokerError::SourceNotConfigured {
                grant_id: grant.grant_id,
                provider_kind: grant.provider_kind,
            });
        };

        if let Some(token) = source.current_token(&grant, (self.now_ms)()).await? {
            return Ok(token);
        }

        let _guard = self
            .locks
            .lock_grant(grant_id)
            .await
            .map_err(|error| AuthBrokerError::Store {
                message: format!("acquire grant renewal lock: {error}"),
            })?;
        // Re-read and re-check under the lock: a concurrent resolver may
        // have renewed while this call waited.
        let grant = self.read_checked_grant(grant_id, audience).await?;
        if let Some(token) = source.current_token(&grant, (self.now_ms)()).await? {
            return Ok(token);
        }
        source.renew_token(&grant, (self.now_ms)()).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::{
        CreateAuthGrantRecord, CreateOAuthClientRecord, InMemoryAuthGrantStore,
        InMemoryGrantLocks, InMemoryOAuthClientStore, InMemorySecretStore, OAuthClientId,
        OAuthTokenResponse, PrincipalRef, SECRET_KIND_STATIC_BEARER, TokenEndpointAuthMethod,
    };

    fn grant_request(grant_id: &str, audience: Option<&str>) -> CreateAuthGrantRecord {
        CreateAuthGrantRecord {
            grant_id: AuthGrantId::new(grant_id),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            principal: PrincipalRef::universe_default(),
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: audience.map(str::to_owned),
            access_token_secret: Some(SecretId::new("authsec_1")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 10,
        }
    }

    async fn broker_with(
        grant: CreateAuthGrantRecord,
        token: Option<&str>,
    ) -> RegistryTokenBroker {
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        grants.create_grant(grant).await.expect("create grant");
        if let Some(token) = token {
            secrets
                .put_secret(PutSecretRecord {
                    secret_id: SecretId::new("authsec_1"),
                    secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
                    value: SecretValue::new(token),
                    created_at_ms: 10,
                })
                .await
                .expect("put secret");
        }
        RegistryTokenBroker::new(grants, secrets, Arc::new(InMemoryGrantLocks::new()))
    }

    fn mcp_audience(resource: &str) -> TokenAudience {
        TokenAudience::McpResource(resource.to_owned())
    }

    #[test]
    fn audience_covering_uses_path_boundaries() {
        assert!(audience_covers(
            "https://crm.example.com/mcp",
            "https://crm.example.com/mcp"
        ));
        assert!(audience_covers(
            "https://crm.example.com",
            "https://crm.example.com/mcp"
        ));
        assert!(audience_covers(
            "https://crm.example.com/mcp",
            "https://crm.example.com/mcp?tenant=1"
        ));
        assert!(!audience_covers(
            "https://crm.example.com/mcp",
            "https://crm.example.com/mcpx"
        ));
        assert!(!audience_covers(
            "https://crm.example.com",
            "https://crm.example.com.evil.com/mcp"
        ));
    }

    #[tokio::test]
    async fn broker_resolves_active_static_bearer_grant() {
        let broker = broker_with(
            grant_request("authgrant_1", Some("https://crm.example.com")),
            Some("token-123"),
        )
        .await;

        let token = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("resolve token");

        assert_eq!(token.expose(), "token-123");
    }

    #[tokio::test]
    async fn broker_rejects_unknown_grant() {
        let broker = broker_with(grant_request("authgrant_1", None), Some("token-123")).await;

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_missing"),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("unknown grant must fail");

        assert!(matches!(error, AuthBrokerError::GrantNotFound { .. }));
    }

    #[tokio::test]
    async fn broker_rejects_revoked_grant() {
        let broker = broker_with(grant_request("authgrant_1", None), Some("token-123")).await;
        broker
            .grants
            .update_grant_status(&AuthGrantId::new("authgrant_1"), AuthGrantStatus::Revoked, 20)
            .await
            .expect("revoke grant");

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("revoked grant must fail");

        assert!(matches!(
            error,
            AuthBrokerError::GrantNotActive {
                status: AuthGrantStatus::Revoked,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn broker_rejects_audience_mismatch() {
        let broker = broker_with(
            grant_request("authgrant_1", Some("https://crm.example.com/mcp")),
            Some("token-123"),
        )
        .await;

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &mcp_audience("https://other.example.com/mcp"),
            )
            .await
            .expect_err("audience mismatch must fail");

        assert!(matches!(error, AuthBrokerError::AudienceMismatch { .. }));
    }

    #[tokio::test]
    async fn broker_rejects_expired_grant_without_refresh_path() {
        let mut request = grant_request("authgrant_1", None);
        request.expires_at_ms = Some(100);
        let broker = broker_with(request, Some("token-123"))
            .await
            .with_now_fn(Arc::new(|| 200));

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("expired grant must fail");

        assert!(matches!(error, AuthBrokerError::GrantExpired { .. }));
    }

    #[tokio::test]
    async fn broker_reports_missing_secret() {
        let broker = broker_with(grant_request("authgrant_1", None), None).await;

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("missing secret must fail");

        assert!(matches!(error, AuthBrokerError::SecretMissing { .. }));
    }

    struct CountingTokenClient {
        responses: Mutex<Vec<Result<OAuthTokenResponse, OAuthTokenError>>>,
        calls: AtomicI64,
    }

    impl CountingTokenClient {
        fn new(responses: Vec<Result<OAuthTokenResponse, OAuthTokenError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: AtomicI64::new(0),
            }
        }
    }

    #[async_trait]
    impl OAuthTokenClient for CountingTokenClient {
        async fn request_token(
            &self,
            _request: &OAuthTokenRequest,
        ) -> Result<OAuthTokenResponse, OAuthTokenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut responses = self.responses.lock().expect("lock");
            if responses.is_empty() {
                return Err(OAuthTokenError::Http {
                    status: None,
                    message: "no scripted response".to_owned(),
                });
            }
            responses.remove(0)
        }
    }

    struct OAuthHarness {
        broker: RegistryTokenBroker,
        grants: Arc<InMemoryAuthGrantStore>,
        secrets: Arc<InMemorySecretStore>,
        token_client: Arc<CountingTokenClient>,
        now: Arc<AtomicI64>,
    }

    /// An OAuth grant whose access token expires at 1_000 with a stored
    /// refresh token, behind a broker whose clock starts at `now`.
    async fn oauth_harness(
        responses: Vec<Result<OAuthTokenResponse, OAuthTokenError>>,
        now: i64,
    ) -> OAuthHarness {
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let clients = Arc::new(InMemoryOAuthClientStore::new());
        clients
            .create_oauth_client(CreateOAuthClientRecord {
                client_id: OAuthClientId::new("crm"),
                provider_id: "crm".to_owned(),
                provider_kind: AuthProviderKind::McpOAuth,
                display_name: None,
                authorization_endpoint: "https://as.example.com/authorize".to_owned(),
                token_endpoint: "https://as.example.com/token".to_owned(),
                remote_client_id: "client-1".to_owned(),
                client_secret: None,
                token_endpoint_auth_method: TokenEndpointAuthMethod::None,
                scopes_default: Vec::new(),
                audience: Some("https://crm.example.com/mcp".to_owned()),
                created_at_ms: 10,
            })
            .await
            .expect("create client");
        grants
            .create_grant(CreateAuthGrantRecord {
                grant_id: AuthGrantId::new("authgrant_oauth"),
                provider_id: "crm".to_owned(),
                provider_kind: AuthProviderKind::McpOAuth,
                principal: PrincipalRef::universe_default(),
                display_name: None,
                subject_hint: None,
                scopes: Vec::new(),
                audience: Some("https://crm.example.com/mcp".to_owned()),
                access_token_secret: Some(SecretId::new("authsec_access")),
                refresh_token_secret: Some(SecretId::new("authsec_refresh")),
                oauth_client: Some(OAuthClientId::new("crm")),
                expires_at_ms: Some(1_000),
                status: AuthGrantStatus::Active,
                metadata: serde_json::Value::Object(Default::default()),
                created_at_ms: 10,
            })
            .await
            .expect("create grant");
        for (id, kind, value) in [
            ("authsec_access", SECRET_KIND_OAUTH_ACCESS_TOKEN, "at-old"),
            ("authsec_refresh", SECRET_KIND_OAUTH_REFRESH_TOKEN, "rt-old"),
        ] {
            secrets
                .put_secret(PutSecretRecord {
                    secret_id: SecretId::new(id),
                    secret_kind: kind.to_owned(),
                    value: SecretValue::new(value),
                    created_at_ms: 10,
                })
                .await
                .expect("put secret");
        }
        let token_client = Arc::new(CountingTokenClient::new(responses));
        let now = Arc::new(AtomicI64::new(now));
        let now_for_fn = now.clone();
        let broker = RegistryTokenBroker::new(
            grants.clone(),
            secrets.clone(),
            Arc::new(InMemoryGrantLocks::new()),
        )
        .with_oauth_refresh(
            OAuthRefreshRuntime::new(clients, token_client.clone()).with_expiry_margin_ms(100),
        )
        .with_now_fn(Arc::new(move || now_for_fn.load(Ordering::SeqCst)));
        OAuthHarness {
            broker,
            grants,
            secrets,
            token_client,
            now,
        }
    }

    fn refreshed_response(access: &str, refresh: Option<&str>) -> OAuthTokenResponse {
        OAuthTokenResponse {
            access_token: SecretValue::new(access),
            token_type: "bearer".to_owned(),
            expires_in_secs: Some(3_600),
            refresh_token: refresh.map(SecretValue::new),
            scope: None,
        }
    }

    fn oauth_grant_id() -> AuthGrantId {
        AuthGrantId::new("authgrant_oauth")
    }

    #[tokio::test]
    async fn fresh_oauth_tokens_resolve_without_refresh() {
        let harness = oauth_harness(Vec::new(), 500).await;

        let token = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("resolve token");

        assert_eq!(token.expose(), "at-old");
        assert_eq!(harness.token_client.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_oauth_tokens_refresh_and_rotate_secrets() {
        let harness = oauth_harness(vec![Ok(refreshed_response("at-new", Some("rt-new")))], 2_000)
            .await;

        let token = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("refresh and resolve");

        assert_eq!(token.expose(), "at-new");
        assert_eq!(harness.token_client.calls.load(Ordering::SeqCst), 1);

        let grant = harness
            .grants
            .read_grant(&oauth_grant_id())
            .await
            .expect("grant");
        assert_eq!(grant.expires_at_ms, Some(2_000 + 3_600_000));
        let access_secret = grant.access_token_secret.expect("access secret");
        let refresh_secret = grant.refresh_token_secret.expect("refresh secret");
        assert_ne!(access_secret.as_str(), "authsec_access");
        assert_ne!(refresh_secret.as_str(), "authsec_refresh");
        let (_, access) = harness
            .secrets
            .read_secret(&access_secret)
            .await
            .expect("new access secret");
        assert_eq!(access.expose(), "at-new");
        let (_, refresh) = harness
            .secrets
            .read_secret(&refresh_secret)
            .await
            .expect("new refresh secret");
        assert_eq!(refresh.expose(), "rt-new");
        // Old secrets are gone.
        assert!(matches!(
            harness
                .secrets
                .read_secret(&SecretId::new("authsec_access"))
                .await,
            Err(AuthRegistryError::SecretNotFound { .. })
        ));
        assert!(matches!(
            harness
                .secrets
                .read_secret(&SecretId::new("authsec_refresh"))
                .await,
            Err(AuthRegistryError::SecretNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn refresh_without_rotation_keeps_the_existing_refresh_token() {
        let harness = oauth_harness(vec![Ok(refreshed_response("at-new", None))], 2_000).await;

        harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("refresh and resolve");

        let grant = harness
            .grants
            .read_grant(&oauth_grant_id())
            .await
            .expect("grant");
        assert_eq!(
            grant.refresh_token_secret,
            Some(SecretId::new("authsec_refresh"))
        );
        let (_, refresh) = harness
            .secrets
            .read_secret(&SecretId::new("authsec_refresh"))
            .await
            .expect("refresh token kept");
        assert_eq!(refresh.expose(), "rt-old");
    }

    #[tokio::test]
    async fn tokens_inside_the_margin_refresh_proactively() {
        // expires at 1_000, margin 100: now=950 is within the margin.
        let harness = oauth_harness(vec![Ok(refreshed_response("at-new", None))], 950).await;

        let token = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("refresh and resolve");

        assert_eq!(token.expose(), "at-new");
        assert_eq!(harness.token_client.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalid_grant_refresh_marks_needs_reauth() {
        let harness = oauth_harness(
            vec![Err(OAuthTokenError::InvalidGrant {
                description: Some("revoked".to_owned()),
            })],
            2_000,
        )
        .await;

        let error = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("dead refresh token must fail");

        assert!(matches!(
            error,
            AuthBrokerError::GrantNotActive {
                status: AuthGrantStatus::NeedsReauth,
                ..
            }
        ));
        let grant = harness
            .grants
            .read_grant(&oauth_grant_id())
            .await
            .expect("grant");
        assert_eq!(grant.status, AuthGrantStatus::NeedsReauth);
    }

    #[tokio::test]
    async fn transient_refresh_failures_fall_back_to_a_still_valid_token() {
        // Inside the margin (token valid until 1_000, now 950): a network
        // failure must not fail the call while the stored token still works.
        let harness = oauth_harness(
            vec![Err(OAuthTokenError::Http {
                status: Some(503),
                message: "unavailable".to_owned(),
            })],
            950,
        )
        .await;

        let token = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("fall back to stored token");

        assert_eq!(token.expose(), "at-old");
    }

    #[tokio::test]
    async fn transient_refresh_failures_error_once_the_token_is_expired() {
        let harness = oauth_harness(
            vec![Err(OAuthTokenError::Http {
                status: Some(503),
                message: "unavailable".to_owned(),
            })],
            2_000,
        )
        .await;

        let error = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("expired token with failing refresh must error");

        assert!(matches!(error, AuthBrokerError::RefreshFailed { .. }));
    }

    #[tokio::test]
    async fn concurrent_resolutions_refresh_single_flight() {
        let harness = oauth_harness(
            vec![
                Ok(refreshed_response("at-new", Some("rt-new"))),
                Ok(refreshed_response("at-second", Some("rt-second"))),
            ],
            2_000,
        )
        .await;
        // After the first refresh the new expiry (2_000 + 3_600_000) makes
        // the token fresh, so the second caller serves it from the store.
        harness.now.store(2_000, Ordering::SeqCst);

        let broker = harness.broker.clone();
        let first = tokio::spawn(async move {
            broker
                .bearer_token(
                    &oauth_grant_id(),
                    &mcp_audience("https://crm.example.com/mcp"),
                )
                .await
        });
        let second = harness
            .broker
            .bearer_token(
                &oauth_grant_id(),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect("second resolution");
        let first = first.await.expect("join").expect("first resolution");

        assert_eq!(first.expose(), "at-new");
        assert_eq!(second.expose(), "at-new");
        assert_eq!(harness.token_client.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn grants_without_a_registered_source_fail_with_a_typed_error() {
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let mut request = grant_request("authgrant_1", None);
        request.provider_kind = AuthProviderKind::GitHubApp;
        request.access_token_secret = None;
        grants.create_grant(request).await.expect("create grant");
        // No source is registered for GitHubApp on a bare broker.
        let broker =
            RegistryTokenBroker::new(grants, secrets, Arc::new(InMemoryGrantLocks::new()));

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &mcp_audience("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("missing source must fail");

        assert!(matches!(
            error,
            AuthBrokerError::SourceNotConfigured {
                provider_kind: AuthProviderKind::GitHubApp,
                ..
            }
        ));
    }
}
