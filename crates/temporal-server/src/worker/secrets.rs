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
use llm_runtime::provider_keys::{ProviderKeyError, ProviderKeyResolver};
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

/// Resolves stored model provider API keys from `model:<provider_id>` auth
/// provider rows (P69 G6). An absent row resolves to `None` so adapters fall
/// back to the env-configured client key; a row that exists but is disabled,
/// of the wrong kind, or missing its credential fails resolution instead of
/// silently falling back.
pub struct StoredProviderKeyResolver {
    providers: Arc<dyn AuthProviderStore>,
    secrets: Arc<dyn SecretStore>,
}

impl StoredProviderKeyResolver {
    pub fn new(providers: Arc<dyn AuthProviderStore>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { providers, secrets }
    }
}

#[async_trait]
impl ProviderKeyResolver for StoredProviderKeyResolver {
    async fn resolve_provider_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedSecretValue>, ProviderKeyError> {
        let row_id = AuthProviderId::try_new(model_auth_provider_id(provider_id)).map_err(
            |error| ProviderKeyError::Backend {
                provider_id: provider_id.to_owned(),
                message: format!("invalid model auth provider id: {error}"),
            },
        )?;
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
        if !matches!(record.config, AuthProviderConfig::ModelApiKey(_)) {
            return Err(ProviderKeyError::NotUsable {
                provider_id: provider_id.to_owned(),
                message: format!(
                    "auth provider {row_id} is kind {:?}, not model_api_key",
                    record.provider_kind
                ),
            });
        }
        if record.status != AuthProviderStatus::Active {
            return Err(ProviderKeyError::NotUsable {
                provider_id: provider_id.to_owned(),
                message: format!("auth provider {row_id} is {:?}", record.status),
            });
        }
        let Some(secret_id) = record.credential_secret else {
            return Err(ProviderKeyError::NotUsable {
                provider_id: provider_id.to_owned(),
                message: format!("auth provider {row_id} has no credential secret"),
            });
        };
        let (_, value) = self.secrets.read_secret(&secret_id).await.map_err(|error| {
            ProviderKeyError::Backend {
                provider_id: provider_id.to_owned(),
                message: format!("read credential secret: {error}"),
            }
        })?;
        Ok(Some(ResolvedSecretValue::new(value.expose())))
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
        StoredProviderKeyResolver::new(providers, secrets)
    }

    #[tokio::test]
    async fn resolves_stored_llm_provider_keys() {
        let resolver = provider_key_resolver(AuthProviderStatus::Active).await;

        let key = resolver
            .resolve_provider_key("openai")
            .await
            .expect("resolve");

        assert_eq!(key.expect("key present").expose(), "stored-api-key");
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
        let resolver = StoredProviderKeyResolver::new(providers, secrets);

        let error = resolver
            .resolve_provider_key("openai")
            .await
            .expect_err("non-llm provider row must fail");

        assert!(matches!(error, ProviderKeyError::NotUsable { .. }));
    }
}
