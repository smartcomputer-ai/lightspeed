use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use thiserror::Error;

use crate::{
    AuthGrantId, AuthGrantStatus, AuthGrantStore, AuthRegistryError, SecretStore, SecretValue,
};

/// The resource a resolved token is about to be sent to. Audience enforcement
/// is the broker's job: a grant bound to an audience only resolves for
/// resources that audience covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenAudience {
    /// Canonical remote MCP server resource URL (RFC 8707 resource).
    McpResource(String),
}

impl TokenAudience {
    fn resource(&self) -> &str {
        match self {
            Self::McpResource(resource) => resource,
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

    #[error("auth broker store failure: {message}")]
    Store { message: String },
}

#[async_trait]
pub trait AuthTokenBroker: Send + Sync {
    /// Resolve a current bearer token for `grant_id`, enforcing grant status,
    /// expiry, and audience. Fails with a typed error; never returns a silent
    /// absence. Optional-auth-absent is a link-policy concern expressed by
    /// omitting `auth_ref` upstream, not by the broker.
    async fn bearer_token(
        &self,
        grant_id: &AuthGrantId,
        audience: &TokenAudience,
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

/// Broker over the registry store traits. G1 resolves static bearer grants;
/// OAuth refresh and provider drivers extend this in later milestones.
#[derive(Clone)]
pub struct RegistryTokenBroker {
    grants: Arc<dyn AuthGrantStore>,
    secrets: Arc<dyn SecretStore>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl RegistryTokenBroker {
    pub fn new(grants: Arc<dyn AuthGrantStore>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            grants,
            secrets,
            now_ms: Arc::new(system_now_ms),
        }
    }

    pub fn with_now_fn(mut self, now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.now_ms = now_ms;
        self
    }
}

fn system_now_ms() -> i64 {
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
        if let Some(expires_at_ms) = grant.expires_at_ms {
            if expires_at_ms <= (self.now_ms)() {
                return Err(AuthBrokerError::GrantExpired {
                    grant_id: grant.grant_id,
                });
            }
        }
        if let Some(grant_audience) = &grant.audience {
            if !audience_covers(grant_audience, audience.resource()) {
                return Err(AuthBrokerError::AudienceMismatch {
                    grant_id: grant.grant_id,
                    requested: audience.resource().to_owned(),
                });
            }
        }

        let Some(secret_id) = &grant.access_token_secret else {
            return Err(AuthBrokerError::SecretMissing {
                grant_id: grant.grant_id,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AuthProviderKind, CreateAuthGrantRecord, InMemoryAuthGrantStore, InMemorySecretStore,
        PrincipalRef, PutSecretRecord, SECRET_KIND_STATIC_BEARER, SecretId,
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
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
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
        RegistryTokenBroker::new(grants, secrets)
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
                &TokenAudience::McpResource("https://crm.example.com/mcp".to_owned()),
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
                &TokenAudience::McpResource("https://crm.example.com/mcp".to_owned()),
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
                &TokenAudience::McpResource("https://crm.example.com/mcp".to_owned()),
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
                &TokenAudience::McpResource("https://other.example.com/mcp".to_owned()),
            )
            .await
            .expect_err("audience mismatch must fail");

        assert!(matches!(error, AuthBrokerError::AudienceMismatch { .. }));
    }

    #[tokio::test]
    async fn broker_rejects_expired_grant() {
        let mut request = grant_request("authgrant_1", None);
        request.expires_at_ms = Some(100);
        let broker = broker_with(request, Some("token-123"))
            .await
            .with_now_fn(Arc::new(|| 200));

        let error = broker
            .bearer_token(
                &AuthGrantId::new("authgrant_1"),
                &TokenAudience::McpResource("https://crm.example.com/mcp".to_owned()),
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
                &TokenAudience::McpResource("https://crm.example.com/mcp".to_owned()),
            )
            .await
            .expect_err("missing secret must fail");

        assert!(matches!(error, AuthBrokerError::SecretMissing { .. }));
    }
}
