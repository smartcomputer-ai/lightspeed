//! Stored model provider credential resolution (P69 G6/G7).
//!
//! Mirrors the [`crate::secrets`] boundary: `llm-runtime` owns this narrow
//! trait and stays free of auth and store dependencies; hosting runtimes adapt
//! their provider/secret stores to it. Resolution happens immediately before a
//! provider request is sent, and the credential travels as a transport header
//! — it never enters materialized or persisted request blobs.
//!
//! `Ok(None)` means "no stored credential for this provider": adapters then
//! fall back to the client's transport-configured key (typically from
//! environment variables). A stored credential that exists but cannot be used
//! (disabled, missing credential) is an error, never a silent fallback.

use std::collections::BTreeMap;

use async_trait::async_trait;
use engine::ModelSelection;
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
pub trait ProviderKeyResolver: Send + Sync {
    async fn resolve_provider_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedProviderAuth>, ProviderKeyError>;
}

/// Resolve the stored credential for the request's provider, mapping failures
/// into the adapter error space. `None` means "use the client-configured key".
pub(crate) async fn resolve_stored_provider_key(
    resolver: &dyn ProviderKeyResolver,
    model: &ModelSelection,
) -> Result<Option<ResolvedProviderAuth>, LlmAdapterError> {
    resolver
        .resolve_provider_key(&model.provider_id)
        .await
        .map_err(|error| LlmAdapterError::ProviderKeyResolution {
            message: error.to_string(),
        })
}

/// Default resolver: no stored credentials exist, so adapters always use the
/// client's transport-configured key.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStoredProviderKeys;

#[async_trait]
impl ProviderKeyResolver for NoStoredProviderKeys {
    async fn resolve_provider_key(
        &self,
        _provider_id: &str,
    ) -> Result<Option<ResolvedProviderAuth>, ProviderKeyError> {
        Ok(None)
    }
}

/// Fixed-map resolver for tests.
#[derive(Clone, Debug, Default)]
pub struct StaticProviderKeys {
    keys: BTreeMap<String, ResolvedProviderAuth>,
}

impl StaticProviderKeys {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_key(mut self, provider_id: impl Into<String>, key: impl Into<String>) -> Self {
        self.keys
            .insert(provider_id.into(), ResolvedProviderAuth::api_key(key));
        self
    }

    pub fn with_bearer(mut self, provider_id: impl Into<String>, token: impl Into<String>) -> Self {
        self.keys
            .insert(provider_id.into(), ResolvedProviderAuth::bearer(token));
        self
    }
}

#[async_trait]
impl ProviderKeyResolver for StaticProviderKeys {
    async fn resolve_provider_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedProviderAuth>, ProviderKeyError> {
        Ok(self.keys.get(provider_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_stored_keys_resolver_returns_none() {
        let resolver = NoStoredProviderKeys;

        let auth = resolver
            .resolve_provider_key("openai")
            .await
            .expect("resolve");

        assert_eq!(auth, None);
    }

    #[tokio::test]
    async fn static_resolver_resolves_known_providers() {
        let resolver = StaticProviderKeys::new()
            .with_key("openai", "key-123")
            .with_bearer("anthropic", "token-456");

        let auth = resolver
            .resolve_provider_key("openai")
            .await
            .expect("resolve")
            .expect("auth present");
        assert_eq!(auth.value.expose(), "key-123");
        assert_eq!(auth.scheme, ProviderAuthScheme::ApiKey);

        let auth = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect("resolve")
            .expect("auth present");
        assert_eq!(auth.value.expose(), "token-456");
        assert_eq!(auth.scheme, ProviderAuthScheme::Bearer);

        let missing = resolver
            .resolve_provider_key("missing")
            .await
            .expect("resolve");
        assert_eq!(missing, None);
    }
}
