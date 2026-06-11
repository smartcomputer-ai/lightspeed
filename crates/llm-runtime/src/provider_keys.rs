//! Stored model provider API key resolution (P69 G6).
//!
//! Mirrors the [`crate::secrets`] boundary: `llm-runtime` owns this narrow
//! trait and stays free of auth and store dependencies; hosting runtimes adapt
//! their provider/secret stores to it. Resolution happens immediately before a
//! provider request is sent, and the key travels as a transport header — it
//! never enters materialized or persisted request blobs.
//!
//! `Ok(None)` means "no stored key for this provider": adapters then fall back
//! to the client's transport-configured key (typically from environment
//! variables). A stored key that exists but cannot be used (disabled, missing
//! credential) is an error, never a silent fallback.

use std::collections::BTreeMap;

use async_trait::async_trait;
use engine::ModelSelection;
use thiserror::Error;

use crate::{error::LlmAdapterError, secrets::ResolvedSecretValue};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderKeyError {
    /// A stored key record exists for the provider but must not be used
    /// (disabled, missing credential). Adapters fail the request instead of
    /// silently falling back to the environment key.
    #[error("stored API key for model provider {provider_id} is not usable: {message}")]
    NotUsable {
        provider_id: String,
        message: String,
    },

    #[error("stored API key lookup failed for model provider {provider_id}: {message}")]
    Backend {
        provider_id: String,
        message: String,
    },
}

/// Resolves the stored API key for a model provider id
/// (`ModelSelection.provider_id`) at provider-send time.
#[async_trait]
pub trait ProviderKeyResolver: Send + Sync {
    async fn resolve_provider_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedSecretValue>, ProviderKeyError>;
}

/// Resolve the stored key for the request's provider, mapping failures into
/// the adapter error space. `None` means "use the client-configured key".
pub(crate) async fn resolve_stored_provider_key(
    resolver: &dyn ProviderKeyResolver,
    model: &ModelSelection,
) -> Result<Option<ResolvedSecretValue>, LlmAdapterError> {
    resolver
        .resolve_provider_key(&model.provider_id)
        .await
        .map_err(|error| LlmAdapterError::ProviderKeyResolution {
            message: error.to_string(),
        })
}

/// Default resolver: no stored keys exist, so adapters always use the
/// client's transport-configured key.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStoredProviderKeys;

#[async_trait]
impl ProviderKeyResolver for NoStoredProviderKeys {
    async fn resolve_provider_key(
        &self,
        _provider_id: &str,
    ) -> Result<Option<ResolvedSecretValue>, ProviderKeyError> {
        Ok(None)
    }
}

/// Fixed-map resolver for tests.
#[derive(Clone, Debug, Default)]
pub struct StaticProviderKeys {
    keys: BTreeMap<String, String>,
}

impl StaticProviderKeys {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_key(mut self, provider_id: impl Into<String>, key: impl Into<String>) -> Self {
        self.keys.insert(provider_id.into(), key.into());
        self
    }
}

#[async_trait]
impl ProviderKeyResolver for StaticProviderKeys {
    async fn resolve_provider_key(
        &self,
        provider_id: &str,
    ) -> Result<Option<ResolvedSecretValue>, ProviderKeyError> {
        Ok(self
            .keys
            .get(provider_id)
            .map(ResolvedSecretValue::new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_stored_keys_resolver_returns_none() {
        let resolver = NoStoredProviderKeys;

        let key = resolver
            .resolve_provider_key("openai")
            .await
            .expect("resolve");

        assert_eq!(key, None);
    }

    #[tokio::test]
    async fn static_resolver_resolves_known_providers() {
        let resolver = StaticProviderKeys::new().with_key("openai", "key-123");

        let key = resolver
            .resolve_provider_key("openai")
            .await
            .expect("resolve");
        assert_eq!(key.expect("key present").expose(), "key-123");

        let missing = resolver
            .resolve_provider_key("anthropic")
            .await
            .expect("resolve");
        assert_eq!(missing, None);
    }
}
