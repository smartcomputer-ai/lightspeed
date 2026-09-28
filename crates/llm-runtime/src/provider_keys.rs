//! Universe model-provider resolution.
//!
//! Mirrors the [`crate::secrets`] boundary: `llm-runtime` owns this narrow
//! trait and stays free of auth and store dependencies; hosting runtimes adapt
//! their provider/secret stores to it. Resolution happens immediately before a
//! provider request is sent, and the credential travels as a transport header
//! — it never enters materialized or persisted request blobs.
//!
//! `Ok(None)` permits deployment fallback only for built-in provider ids.
//! Custom providers without a universe record fail before network I/O.

use std::collections::BTreeMap;

use async_trait::async_trait;
use engine::{ModelSelection, ProviderApiKind};
use llm_clients::EndpointOverride;
use thiserror::Error;

use crate::{error::LlmAdapterError, secrets::ResolvedSecretValue};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderKeyError {
    /// A stored credential record exists for the provider but must not be
    /// used (disabled, missing credential, unusable grant). Adapters fail the
    /// request instead of silently falling back to the environment key.
    #[error("stored credential for model provider {provider_id} is not usable: {message}")]
    NotUsable {
        provider_id: String,
        message: String,
    },

    #[error("stored credential lookup failed for model provider {provider_id}: {message}")]
    Backend {
        provider_id: String,
        message: String,
    },
}

/// How a resolved provider credential is sent on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderAuthScheme {
    /// Provider API key in the provider's native key header.
    ApiKey,
    /// OAuth access token as `Authorization: Bearer` (plus provider OAuth
    /// beta headers where required).
    Bearer,
}

/// A resolved provider credential plus the scheme it must be sent with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProviderAuth {
    pub value: ResolvedSecretValue,
    pub scheme: ProviderAuthScheme,
}

/// Validated per-provider endpoint plus its admitted API kinds.
#[derive(Clone, Debug)]
pub struct ResolvedEndpoint {
    pub transport: EndpointOverride,
    pub api_kinds: BTreeMap<String, ()>,
}

impl ResolvedEndpoint {
    pub fn new(
        base_url: &str,
        headers: &BTreeMap<String, String>,
        api_kinds: impl IntoIterator<Item = String>,
    ) -> Result<Self, llm_clients::LlmApiError> {
        Ok(Self {
            transport: EndpointOverride::from_parts(base_url, headers)?,
            api_kinds: api_kinds.into_iter().map(|kind| (kind, ())).collect(),
        })
    }

    pub fn supports(&self, api_kind: &str) -> bool {
        self.api_kinds.contains_key(api_kind)
    }
}

/// Authentication and transport selected for one model provider.
#[derive(Clone, Debug)]
pub struct ResolvedModelProvider {
    pub auth: Option<ResolvedProviderAuth>,
    pub endpoint: Option<ResolvedEndpoint>,
}

impl ResolvedModelProvider {
    pub fn as_request_auth(&self) -> llm_clients::RequestAuth<'_> {
        self.auth
            .as_ref()
            .map(ResolvedProviderAuth::as_request_auth)
            .unwrap_or(llm_clients::RequestAuth::None)
    }
}

impl ResolvedProviderAuth {
    pub fn api_key(value: impl Into<String>) -> Self {
        Self {
            value: ResolvedSecretValue::new(value),
            scheme: ProviderAuthScheme::ApiKey,
        }
    }

    pub fn bearer(value: impl Into<String>) -> Self {
        Self {
            value: ResolvedSecretValue::new(value),
            scheme: ProviderAuthScheme::Bearer,
        }
    }

    pub fn as_request_auth(&self) -> llm_clients::RequestAuth<'_> {
        match self.scheme {
            ProviderAuthScheme::ApiKey => llm_clients::RequestAuth::ApiKey(self.value.expose()),
            ProviderAuthScheme::Bearer => llm_clients::RequestAuth::Bearer(self.value.expose()),
        }
    }
}

/// Resolves the stored credential for a model provider id
/// (`ModelSelection.provider_id`) at provider-send time.
#[async_trait]
pub trait ModelProviderResolver: Send + Sync {
    async fn resolve_model_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedModelProvider>, ProviderKeyError>;
}

/// Resolve the stored credential for the request's provider, mapping failures
/// into the adapter error space. `None` means "use the client-configured key".
pub(crate) async fn resolve_model_provider(
    resolver: &dyn ModelProviderResolver,
    model: &ModelSelection,
) -> Result<Option<ResolvedModelProvider>, LlmAdapterError> {
    resolve_provider_route(
        resolver,
        &model.provider_id,
        provider_api_kind_name(&model.api_kind),
    )
    .await
    .map_err(|error| LlmAdapterError::ProviderKeyResolution {
        message: error.to_string(),
    })
}

/// Resolve a protocol route without extending the engine's generation model enum.
/// Only built-in providers may use deployment transport defaults.
pub async fn resolve_provider_route(
    resolver: &dyn ModelProviderResolver,
    provider_id: &str,
    api_kind: &str,
) -> Result<Option<ResolvedModelProvider>, ProviderKeyError> {
    let resolved = resolver.resolve_model_provider(provider_id).await?;
    if resolved.is_none() && !is_builtin_provider(provider_id) {
        return Err(ProviderKeyError::NotUsable {
            provider_id: provider_id.into(),
            message: format!(
                "custom model provider {} has no universe model-provider record",
                provider_id
            ),
        });
    }
    if !is_builtin_provider(provider_id)
        && resolved
            .as_ref()
            .is_some_and(|provider| provider.endpoint.is_none())
    {
        return Err(ProviderKeyError::NotUsable {
            provider_id: provider_id.into(),
            message: format!(
                "custom model provider {} has no endpoint configuration",
                provider_id
            ),
        });
    }
    if let Some(endpoint) = resolved
        .as_ref()
        .and_then(|provider| provider.endpoint.as_ref())
        && !endpoint.supports(api_kind)
    {
        return Err(ProviderKeyError::NotUsable {
            provider_id: provider_id.into(),
            message: format!(
                "model provider {} endpoint does not admit API kind {}",
                provider_id, api_kind
            ),
        });
    }
    Ok(resolved)
}

fn is_builtin_provider(provider_id: &str) -> bool {
    matches!(provider_id, "openai" | "anthropic")
}

fn provider_api_kind_name(api_kind: &ProviderApiKind) -> &'static str {
    match api_kind {
        ProviderApiKind::OpenAiResponses => "openai:responses",
        ProviderApiKind::OpenAiCompletions => "openai:completions",
        ProviderApiKind::AnthropicMessages => "anthropic:messages",
    }
}

/// Default resolver: no stored credentials exist, so adapters always use the
/// client's transport-configured key.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStoredModelProviders;

#[async_trait]
impl ModelProviderResolver for NoStoredModelProviders {
    async fn resolve_model_provider(
        &self,
        _provider_id: &str,
    ) -> Result<Option<ResolvedModelProvider>, ProviderKeyError> {
        Ok(None)
    }
}

/// Fixed-map resolver for tests.
#[derive(Clone, Debug, Default)]
pub struct StaticModelProviders {
    providers: BTreeMap<String, ResolvedModelProvider>,
}

impl StaticModelProviders {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_key(mut self, provider_id: impl Into<String>, key: impl Into<String>) -> Self {
        self.providers.insert(
            provider_id.into(),
            ResolvedModelProvider {
                auth: Some(ResolvedProviderAuth::api_key(key)),
                endpoint: None,
            },
        );
        self
    }

    pub fn with_bearer(mut self, provider_id: impl Into<String>, token: impl Into<String>) -> Self {
        self.providers.insert(
            provider_id.into(),
            ResolvedModelProvider {
                auth: Some(ResolvedProviderAuth::bearer(token)),
                endpoint: None,
            },
        );
        self
    }

    pub fn with_provider(
        mut self,
        provider_id: impl Into<String>,
        provider: ResolvedModelProvider,
    ) -> Self {
        self.providers.insert(provider_id.into(), provider);
        self
    }
}

#[async_trait]
impl ModelProviderResolver for StaticModelProviders {
    async fn resolve_model_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedModelProvider>, ProviderKeyError> {
        Ok(self.providers.get(provider_id).cloned())
    }
}

pub type NoStoredProviderKeys = NoStoredModelProviders;
pub type StaticProviderKeys = StaticModelProviders;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_stored_keys_resolver_returns_none() {
        let resolver = NoStoredModelProviders;

        let auth = resolver
            .resolve_model_provider("openai")
            .await
            .expect("resolve");

        assert!(auth.is_none());
    }

    #[tokio::test]
    async fn static_resolver_resolves_known_providers() {
        let resolver = StaticModelProviders::new()
            .with_key("openai", "key-123")
            .with_bearer("anthropic", "token-456");

        let auth = resolver
            .resolve_model_provider("openai")
            .await
            .expect("resolve")
            .expect("auth present");
        let auth = auth.auth.expect("auth");
        assert_eq!(auth.value.expose(), "key-123");
        assert_eq!(auth.scheme, ProviderAuthScheme::ApiKey);

        let auth = resolver
            .resolve_model_provider("anthropic")
            .await
            .expect("resolve")
            .expect("auth present");
        let auth = auth.auth.expect("auth");
        assert_eq!(auth.value.expose(), "token-456");
        assert_eq!(auth.scheme, ProviderAuthScheme::Bearer);

        let missing = resolver
            .resolve_model_provider("missing")
            .await
            .expect("resolve");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn custom_provider_requires_a_row_and_endpoint_admits_the_selected_api_kind() {
        let missing = resolve_model_provider(
            &NoStoredModelProviders,
            &ModelSelection {
                api_kind: ProviderApiKind::OpenAiCompletions,
                provider_id: "openrouter".to_owned(),
                model: "model".to_owned(),
            },
        )
        .await;
        assert!(matches!(
            missing,
            Err(LlmAdapterError::ProviderKeyResolution { .. })
        ));

        let key_only = StaticModelProviders::new().with_key("openrouter", "key");
        let missing_endpoint = resolve_model_provider(
            &key_only,
            &ModelSelection {
                api_kind: ProviderApiKind::OpenAiCompletions,
                provider_id: "openrouter".to_owned(),
                model: "model".to_owned(),
            },
        )
        .await;
        assert!(matches!(
            missing_endpoint,
            Err(LlmAdapterError::ProviderKeyResolution { .. })
        ));

        let endpoint = ResolvedEndpoint::new(
            "http://127.0.0.1:8080/v1",
            &BTreeMap::new(),
            ["openai:completions".to_owned()],
        )
        .expect("endpoint");
        let resolver = StaticModelProviders::new().with_provider(
            "openrouter",
            ResolvedModelProvider {
                auth: None,
                endpoint: Some(endpoint),
            },
        );
        let accepted = resolve_model_provider(
            &resolver,
            &ModelSelection {
                api_kind: ProviderApiKind::OpenAiCompletions,
                provider_id: "openrouter".to_owned(),
                model: "model".to_owned(),
            },
        )
        .await
        .expect("accepted")
        .expect("provider");
        assert!(matches!(
            accepted.as_request_auth(),
            llm_clients::RequestAuth::None
        ));

        let rejected = resolve_model_provider(
            &resolver,
            &ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openrouter".to_owned(),
                model: "model".to_owned(),
            },
        )
        .await;
        assert!(matches!(
            rejected,
            Err(LlmAdapterError::ProviderKeyResolution { .. })
        ));
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn transcription_routes_require_matching_explicit_custom_endpoints() {
        assert!(
            resolve_provider_route(
                &NoStoredModelProviders,
                "openai",
                "openai:audio-transcriptions"
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(matches!(
            resolve_provider_route(
                &NoStoredModelProviders,
                "custom",
                "openai:audio-transcriptions"
            )
            .await,
            Err(ProviderKeyError::NotUsable { .. })
        ));
        let route = ResolvedModelProvider {
            auth: None,
            endpoint: Some(
                ResolvedEndpoint::new(
                    "http://localhost:9999/v1",
                    &BTreeMap::new(),
                    ["openai:audio-transcriptions".into()],
                )
                .unwrap(),
            ),
        };
        struct Resolver(ResolvedModelProvider);
        #[async_trait]
        impl ModelProviderResolver for Resolver {
            async fn resolve_model_provider(
                &self,
                _: &str,
            ) -> Result<Option<ResolvedModelProvider>, ProviderKeyError> {
                Ok(Some(self.0.clone()))
            }
        }
        let resolver = Resolver(route);
        let resolved = resolve_provider_route(&resolver, "custom", "openai:audio-transcriptions")
            .await
            .unwrap()
            .unwrap();
        assert!(resolved.auth.is_none());
        assert!(matches!(
            resolve_provider_route(&resolver, "custom", "openai:responses").await,
            Err(ProviderKeyError::NotUsable { .. })
        ));
    }
}
