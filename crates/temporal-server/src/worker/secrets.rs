//! Broker-backed secret resolution for the worker LLM runtime.
//!
//! Adapts the P69 token broker to `llm-runtime`'s [`SecretResolver`] boundary.
//! Resolution runs only inside activity execution; resolved values never enter
//! Temporal history, engine events, or persisted provider request blobs.

use std::sync::Arc;

use async_trait::async_trait;
use auth_registry::{
    AuthBrokerError, AuthGrantId, AuthProviderConfig, AuthProviderId, AuthProviderStatus,
    AuthProviderStore, AuthRegistryError, AuthTokenBroker, SecretStore, TokenAudience,
    model_auth_provider_id,
};
use engine::SecretRef;
use llm_runtime::provider_keys::{ProviderKeyError, ProviderKeyResolver, ResolvedProviderAuth};
use llm_runtime::secrets::{
    EnvSecretResolver, ResolvedSecretValue, SECRET_NAMESPACE_AUTH_GRANT, SECRET_NAMESPACE_ENV,
    SecretResolveError, SecretResolver,
};

/// Dispatches on `SecretRef.namespace`: `auth_grant` resolves through the
/// token broker with audience enforcement; `env` falls back to environment
/// variables for development.
pub struct BrokerSecretResolver {
    broker: Arc<dyn AuthTokenBroker>,
    env: EnvSecretResolver,
}

impl BrokerSecretResolver {
    pub fn new(broker: Arc<dyn AuthTokenBroker>) -> Self {
        Self {
            broker,
            env: EnvSecretResolver,
        }
    }
}

#[async_trait]
impl SecretResolver for BrokerSecretResolver {
    async fn resolve(
        &self,
        secret_ref: &SecretRef,
        audience: Option<&str>,
    ) -> Result<ResolvedSecretValue, SecretResolveError> {
        match secret_ref.namespace.as_str() {
            SECRET_NAMESPACE_AUTH_GRANT => {
                let grant_id = AuthGrantId::try_new(secret_ref.id.clone()).map_err(|error| {
                    SecretResolveError::Backend {
                        namespace: secret_ref.namespace.clone(),
                        id: secret_ref.id.clone(),
                        message: format!("invalid auth grant id: {error}"),
                    }
                })?;
                let Some(audience) = audience else {
                    return Err(SecretResolveError::Backend {
                        namespace: secret_ref.namespace.clone(),
                        id: secret_ref.id.clone(),
                        message: "auth_grant resolution requires a target audience".to_owned(),
                    });
                };
                let token = self
                    .broker
                    .bearer_token(&grant_id, &TokenAudience::McpResource(audience.to_owned()))
                    .await
                    .map_err(|error| broker_error_to_resolve_error(secret_ref, error))?;
                Ok(ResolvedSecretValue::new(token.expose()))
            }
            SECRET_NAMESPACE_ENV => self.env.resolve(secret_ref, audience).await,
            other => Err(SecretResolveError::UnsupportedNamespace {
                namespace: other.to_owned(),
            }),
        }
    }
}

/// Resolves stored model provider credentials from `model:<provider_id>`
/// auth provider rows (P69 G6/G7). An absent row resolves to `None` so
/// adapters fall back to the env-configured client key; a row that exists but
/// is disabled, of the wrong kind, missing its credential, or bound to an
/// unusable grant fails resolution instead of silently falling back.
///
/// Two row kinds resolve: `model_api_key` reads the row's encrypted
/// credential secret (sent in the provider's native key header), and
/// `model_oauth` resolves the bound grant through the token broker (refresh
/// included) and sends it as an OAuth bearer token.
pub struct StoredProviderKeyResolver {
    providers: Arc<dyn AuthProviderStore>,
    secrets: Arc<dyn SecretStore>,
    broker: Arc<dyn AuthTokenBroker>,
}

impl StoredProviderKeyResolver {
    pub fn new(
        providers: Arc<dyn AuthProviderStore>,
        secrets: Arc<dyn SecretStore>,
        broker: Arc<dyn AuthTokenBroker>,
    ) -> Self {
        Self {
            providers,
            secrets,
            broker,
        }
    }
}

#[async_trait]
impl ProviderKeyResolver for StoredProviderKeyResolver {
    async fn resolve_provider_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedProviderAuth>, ProviderKeyError> {
        let row_id =
            AuthProviderId::try_new(model_auth_provider_id(provider_id)).map_err(|error| {
                ProviderKeyError::Backend {
                    provider_id: provider_id.to_owned(),
                    message: format!("invalid model auth provider id: {error}"),
                }
            })?;
        let record = match self.providers.read_auth_provider(&row_id).await {
            Ok(record) => record,
            Err(AuthRegistryError::ProviderNotFound { .. }) => return Ok(None),
            Err(error) => {
                return Err(ProviderKeyError::Backend {
                    provider_id: provider_id.to_owned(),
                    message: error.to_string(),
                });
            }
        };
        if record.status != AuthProviderStatus::Active {
            return Err(ProviderKeyError::NotUsable {
                provider_id: provider_id.to_owned(),
                message: format!("auth provider {row_id} is {:?}", record.status),
            });
        }
        match &record.config {
            AuthProviderConfig::ModelApiKey(_) => {
                let Some(secret_id) = &record.credential_secret else {
                    return Err(ProviderKeyError::NotUsable {
                        provider_id: provider_id.to_owned(),
                        message: format!("auth provider {row_id} has no credential secret"),
                    });
                };
                let (_, value) = self.secrets.read_secret(secret_id).await.map_err(|error| {
                    ProviderKeyError::Backend {
                        provider_id: provider_id.to_owned(),
                        message: format!("read credential secret: {error}"),
                    }
                })?;
                Ok(Some(ResolvedProviderAuth::api_key(value.expose())))
            }
            AuthProviderConfig::ModelOAuth(config) => {
                let audience = config
                    .audience
                    .clone()
                    .unwrap_or_else(|| model_auth_provider_id(provider_id));
                let token = self
                    .broker
                    .bearer_token(&config.grant_id, &TokenAudience::ModelProvider(audience))
                    .await
                    .map_err(|error| match error {
                        AuthBrokerError::Store { message } => ProviderKeyError::Backend {
                            provider_id: provider_id.to_owned(),
                            message,
                        },
                        other => ProviderKeyError::NotUsable {
                            provider_id: provider_id.to_owned(),
                            message: other.to_string(),
                        },
                    })?;
                Ok(Some(ResolvedProviderAuth::bearer(token.expose())))
            }
            other => Err(ProviderKeyError::NotUsable {
                provider_id: provider_id.to_owned(),
                message: format!(
                    "auth provider {row_id} is kind {:?}, not a model provider credential",
                    other.provider_kind()
                ),
            }),
        }
    }
}

fn broker_error_to_resolve_error(
    secret_ref: &SecretRef,
    error: AuthBrokerError,
) -> SecretResolveError {
    match error {
        AuthBrokerError::GrantNotFound { .. } => SecretResolveError::NotFound {
            namespace: secret_ref.namespace.clone(),
            id: secret_ref.id.clone(),
        },
        other => SecretResolveError::Backend {
            namespace: secret_ref.namespace.clone(),
            id: secret_ref.id.clone(),
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use auth_registry::{
        AuthGrantStatus, AuthGrantStore, AuthProviderKind, CreateAuthGrantRecord,
        InMemoryAuthGrantStore, InMemoryGrantLocks, InMemorySecretStore, PrincipalRef,
        PutSecretRecord, RegistryTokenBroker, SECRET_KIND_STATIC_BEARER, SecretId, SecretStore,
        SecretValue,
    };

    use super::*;

    async fn resolver_with_grant(audience: Option<&str>) -> BrokerSecretResolver {
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        grants
            .create_grant(CreateAuthGrantRecord {
                grant_id: AuthGrantId::new("authgrant_1"),
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
            })
            .await
            .expect("create grant");
        secrets
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_1"),
                secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
                value: SecretValue::new("token-123"),
                created_at_ms: 10,
            })
            .await
            .expect("put secret");
        BrokerSecretResolver::new(Arc::new(RegistryTokenBroker::new(
            grants,
            secrets,
            Arc::new(InMemoryGrantLocks::new()),
        )))
    }

    fn auth_grant_ref(id: &str) -> SecretRef {
        SecretRef {
            namespace: SECRET_NAMESPACE_AUTH_GRANT.to_owned(),
            id: id.to_owned(),
        }
    }

    #[tokio::test]
    async fn resolves_auth_grant_refs_through_the_broker() {
        let resolver = resolver_with_grant(Some("https://crm.example.com")).await;

        let value = resolver
            .resolve(
                &auth_grant_ref("authgrant_1"),
                Some("https://crm.example.com/mcp"),
            )
            .await
            .expect("resolve grant");

        assert_eq!(value.expose(), "token-123");
    }

    #[tokio::test]
    async fn requires_an_audience_for_auth_grant_refs() {
        let resolver = resolver_with_grant(None).await;

        let error = resolver
            .resolve(&auth_grant_ref("authgrant_1"), None)
            .await
            .expect_err("missing audience must fail");

        assert!(matches!(error, SecretResolveError::Backend { .. }));
    }

    #[tokio::test]
    async fn maps_unknown_grants_to_not_found() {
        let resolver = resolver_with_grant(None).await;

        let error = resolver
            .resolve(
                &auth_grant_ref("authgrant_missing"),
                Some("https://crm.example.com/mcp"),
            )
            .await
            .expect_err("unknown grant must fail");

        assert!(matches!(error, SecretResolveError::NotFound { .. }));
    }

    #[tokio::test]
    async fn rejects_unknown_namespaces() {
        let resolver = resolver_with_grant(None).await;

        let error = resolver
            .resolve(
                &SecretRef {
                    namespace: "vault".to_owned(),
                    id: "x".to_owned(),
                },
                None,
            )
            .await
            .expect_err("unknown namespace must fail");

        assert!(matches!(
            error,
            SecretResolveError::UnsupportedNamespace { .. }
        ));
    }

    /// Broker over empty stores: good enough for the api-key paths, which
    /// never consult it.
    fn empty_broker() -> Arc<dyn AuthTokenBroker> {
        Arc::new(RegistryTokenBroker::new(
            Arc::new(InMemoryAuthGrantStore::new()),
            Arc::new(InMemorySecretStore::new()),
            Arc::new(InMemoryGrantLocks::new()),
        ))
    }

    async fn provider_key_resolver(
        status: auth_registry::AuthProviderStatus,
    ) -> StoredProviderKeyResolver {
        let providers = Arc::new(auth_registry::InMemoryAuthProviderStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_llm"),
                secret_kind: auth_registry::SECRET_KIND_MODEL_API_KEY.to_owned(),
                value: SecretValue::new("stored-api-key"),
                created_at_ms: 10,
            })
            .await
            .expect("put secret");
        providers
            .create_auth_provider(auth_registry::CreateAuthProviderRecord {
                provider_id: AuthProviderId::new(model_auth_provider_id("openai")),
                display_name: None,
                config: AuthProviderConfig::ModelApiKey(auth_registry::ModelApiKeyConfig::default()),
                credential_secret: Some(SecretId::new("authsec_llm")),
                status,
                created_at_ms: 10,
            })
            .await
            .expect("create provider");
        StoredProviderKeyResolver::new(providers, secrets, empty_broker())
    }

    #[tokio::test]
    async fn resolves_stored_llm_provider_keys() {
        let resolver = provider_key_resolver(AuthProviderStatus::Active).await;

        let auth = resolver
            .resolve_provider_key("openai")
            .await
            .expect("resolve")
            .expect("auth present");

        assert_eq!(auth.value.expose(), "stored-api-key");
        assert_eq!(
            auth.scheme,
            llm_runtime::provider_keys::ProviderAuthScheme::ApiKey
        );
    }

    #[tokio::test]
    async fn absent_llm_provider_rows_resolve_to_none() {
        let resolver = provider_key_resolver(AuthProviderStatus::Active).await;

        let key = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect("resolve");

        assert_eq!(key, None);
    }

    #[tokio::test]
    async fn disabled_llm_provider_rows_fail_instead_of_falling_back() {
        let resolver = provider_key_resolver(AuthProviderStatus::Disabled).await;

        let error = resolver
            .resolve_provider_key("openai")
            .await
            .expect_err("disabled provider must fail");

        assert!(matches!(error, ProviderKeyError::NotUsable { .. }));
    }

    #[tokio::test]
    async fn non_llm_provider_rows_fail_with_a_kind_error() {
        let providers = Arc::new(auth_registry::InMemoryAuthProviderStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_pem"),
                secret_kind: auth_registry::SECRET_KIND_GITHUB_APP_PRIVATE_KEY.to_owned(),
                value: SecretValue::new("pem"),
                created_at_ms: 10,
            })
            .await
            .expect("put secret");
        providers
            .create_auth_provider(auth_registry::CreateAuthProviderRecord {
                provider_id: AuthProviderId::new("model:openai"),
                display_name: None,
                config: AuthProviderConfig::GitHubApp(auth_registry::GitHubAppConfig {
                    app_id: "12345".to_owned(),
                    api_base_url: "https://api.github.com".to_owned(),
                }),
                credential_secret: Some(SecretId::new("authsec_pem")),
                status: AuthProviderStatus::Active,
                created_at_ms: 10,
            })
            .await
            .expect("create provider");
        let resolver = StoredProviderKeyResolver::new(providers, secrets, empty_broker());

        let error = resolver
            .resolve_provider_key("openai")
            .await
            .expect_err("non-llm provider row must fail");

        assert!(matches!(error, ProviderKeyError::NotUsable { .. }));
    }

    /// Build a resolver with a `model_oauth` row bound to a grant whose
    /// access token lives in the shared secret store.
    async fn model_oauth_resolver(
        grant_audience: Option<&str>,
        binding_audience: Option<&str>,
    ) -> StoredProviderKeyResolver {
        let providers = Arc::new(auth_registry::InMemoryAuthProviderStore::new());
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        grants
            .create_grant(CreateAuthGrantRecord {
                grant_id: AuthGrantId::new("authgrant_model"),
                provider_id: "custom".to_owned(),
                provider_kind: AuthProviderKind::CustomOAuth,
                principal: PrincipalRef::universe_default(),
                display_name: None,
                subject_hint: None,
                scopes: Vec::new(),
                audience: grant_audience.map(str::to_owned),
                access_token_secret: Some(SecretId::new("authsec_access")),
                refresh_token_secret: None,
                oauth_client: None,
                expires_at_ms: None,
                status: AuthGrantStatus::Active,
                metadata: serde_json::Value::Object(Default::default()),
                created_at_ms: 10,
            })
            .await
            .expect("create grant");
        secrets
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_access"),
                secret_kind: "auth.oauth.access_token".to_owned(),
                value: SecretValue::new("oauth-access-token"),
                created_at_ms: 10,
            })
            .await
            .expect("put secret");
        providers
            .create_auth_provider(auth_registry::CreateAuthProviderRecord {
                provider_id: AuthProviderId::new(model_auth_provider_id("anthropic")),
                display_name: None,
                config: AuthProviderConfig::ModelOAuth(auth_registry::ModelOAuthConfig {
                    grant_id: AuthGrantId::new("authgrant_model"),
                    audience: binding_audience.map(str::to_owned),
                }),
                credential_secret: None,
                status: AuthProviderStatus::Active,
                created_at_ms: 10,
            })
            .await
            .expect("create provider");
        let broker: Arc<dyn AuthTokenBroker> = Arc::new(RegistryTokenBroker::new(
            grants,
            secrets.clone(),
            Arc::new(InMemoryGrantLocks::new()),
        ));
        StoredProviderKeyResolver::new(providers, secrets, broker)
    }

    #[tokio::test]
    async fn model_oauth_rows_resolve_grant_tokens_as_bearer_auth() {
        let resolver = model_oauth_resolver(
            Some("https://api.anthropic.com"),
            Some("https://api.anthropic.com"),
        )
        .await;

        let auth = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect("resolve")
            .expect("auth present");

        assert_eq!(auth.value.expose(), "oauth-access-token");
        assert_eq!(
            auth.scheme,
            llm_runtime::provider_keys::ProviderAuthScheme::Bearer
        );
    }

    #[tokio::test]
    async fn model_oauth_bindings_without_audience_only_cover_unrestricted_grants() {
        let resolver = model_oauth_resolver(None, None).await;
        let auth = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect("resolve")
            .expect("auth present");
        assert_eq!(auth.value.expose(), "oauth-access-token");

        // An audience-bound grant must not resolve through an audience-less
        // binding: the sentinel `model:<provider_id>` is not a URL the grant
        // audience can cover.
        let resolver = model_oauth_resolver(Some("https://api.anthropic.com"), None).await;
        let error = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect_err("audience-bound grant must fail without binding audience");
        assert!(matches!(error, ProviderKeyError::NotUsable { .. }));
    }

    #[tokio::test]
    async fn model_oauth_bindings_fail_on_audience_mismatch() {
        let resolver = model_oauth_resolver(
            Some("https://api.anthropic.com"),
            Some("https://api.openai.com"),
        )
        .await;

        let error = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect_err("mismatched audience must fail");

        assert!(matches!(error, ProviderKeyError::NotUsable { .. }));
    }
}
