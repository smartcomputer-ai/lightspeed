//! Runtime secret resolution for provider request auth injection.
//!
//! `llm-runtime` owns this narrow boundary and stays free of auth-store
//! dependencies. Hosting runtimes adapt their token broker to [`SecretResolver`]
//! and dispatch on `SecretRef.namespace` (`auth_grant` -> broker, `env` ->
//! environment lookup for development). Resolution happens immediately before
//! a provider request is sent; resolved values never enter persisted request
//! blobs, which carry `<redacted>` placeholders instead.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use engine::SecretRef;
use thiserror::Error;

pub const SECRET_NAMESPACE_ENV: &str = "env";
pub const SECRET_NAMESPACE_AUTH_GRANT: &str = "auth_grant";

/// Placeholder written into persisted provider request blobs where a resolved
/// auth value was injected into the sent request.
pub const REDACTED_SECRET_PLACEHOLDER: &str = "<redacted>";

/// A resolved secret. `Debug` output is redacted and the type is not
/// serializable; read the value with [`ResolvedSecretValue::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedSecretValue(String);

impl ResolvedSecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedSecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedSecretValue(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SecretResolveError {
    #[error("secret not found: {namespace}:{id}")]
    NotFound { namespace: String, id: String },

    #[error("unsupported secret namespace: {namespace}")]
    UnsupportedNamespace { namespace: String },

    #[error("secret resolution failed for {namespace}:{id}: {message}")]
    Backend {
        namespace: String,
        id: String,
        message: String,
    },
}

/// Resolves a [`SecretRef`] to a secret value at provider-send time.
///
/// A present `auth_ref` means auth is required for that spec, so this returns
/// the value or a typed error; it never signals absence with an `Option`.
/// Optional auth is expressed upstream by omitting `auth_ref` entirely.
/// `audience` carries the resource the value will be sent to (for remote MCP,
/// the server URL) so audience-enforcing resolvers can refuse mismatches.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(
        &self,
        secret_ref: &SecretRef,
        audience: Option<&str>,
    ) -> Result<ResolvedSecretValue, SecretResolveError>;
}

/// Default resolver: fails every resolution. Adapters constructed without an
/// explicit resolver keep failing clearly before provider I/O instead of
/// silently sending unauthenticated requests.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnconfiguredSecretResolver;

#[async_trait]
impl SecretResolver for UnconfiguredSecretResolver {
    async fn resolve(
        &self,
        secret_ref: &SecretRef,
        _audience: Option<&str>,
    ) -> Result<ResolvedSecretValue, SecretResolveError> {
        Err(SecretResolveError::Backend {
            namespace: secret_ref.namespace.clone(),
            id: secret_ref.id.clone(),
            message: "no secret resolver configured for this runtime".to_owned(),
        })
    }
}

/// Development resolver for the `env` namespace: resolves `SecretRef.id` as an
/// environment variable name. Other namespaces fail with
/// [`SecretResolveError::UnsupportedNamespace`].
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvSecretResolver;

#[async_trait]
impl SecretResolver for EnvSecretResolver {
    async fn resolve(
        &self,
        secret_ref: &SecretRef,
        _audience: Option<&str>,
    ) -> Result<ResolvedSecretValue, SecretResolveError> {
        if secret_ref.namespace != SECRET_NAMESPACE_ENV {
            return Err(SecretResolveError::UnsupportedNamespace {
                namespace: secret_ref.namespace.clone(),
            });
        }
        match std::env::var(&secret_ref.id) {
            Ok(value) if !value.is_empty() => Ok(ResolvedSecretValue::new(value)),
            _ => Err(SecretResolveError::NotFound {
                namespace: secret_ref.namespace.clone(),
                id: secret_ref.id.clone(),
            }),
        }
    }
}

/// Fixed-map resolver for tests.
#[derive(Clone, Debug, Default)]
pub struct StaticSecretResolver {
    values: BTreeMap<(String, String), String>,
}

impl StaticSecretResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_secret(
        mut self,
        namespace: impl Into<String>,
        id: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.values
            .insert((namespace.into(), id.into()), value.into());
        self
    }
}

#[async_trait]
impl SecretResolver for StaticSecretResolver {
    async fn resolve(
        &self,
        secret_ref: &SecretRef,
        _audience: Option<&str>,
    ) -> Result<ResolvedSecretValue, SecretResolveError> {
        self.values
            .get(&(secret_ref.namespace.clone(), secret_ref.id.clone()))
            .map(ResolvedSecretValue::new)
            .ok_or_else(|| SecretResolveError::NotFound {
                namespace: secret_ref.namespace.clone(),
                id: secret_ref.id.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_secret_values_redact_debug_output() {
        let value = ResolvedSecretValue::new("token-123");

        let debug = format!("{value:?}");

        assert!(!debug.contains("token-123"));
        assert!(debug.contains("<redacted>"));
    }

    #[tokio::test]
    async fn unconfigured_resolver_fails_with_backend_error() {
        let resolver = UnconfiguredSecretResolver;

        let error = resolver
            .resolve(
                &SecretRef {
                    namespace: "auth_grant".to_owned(),
                    id: "authgrant_1".to_owned(),
                },
                None,
            )
            .await
            .expect_err("unconfigured resolver must fail");

        assert!(matches!(error, SecretResolveError::Backend { .. }));
    }

    #[tokio::test]
    async fn static_resolver_resolves_known_refs() {
        let resolver = StaticSecretResolver::new().with_secret("auth_grant", "g1", "token-123");

        let value = resolver
            .resolve(
                &SecretRef {
                    namespace: "auth_grant".to_owned(),
                    id: "g1".to_owned(),
                },
                Some("https://crm.example.com/mcp"),
            )
            .await
            .expect("resolve known ref");
        assert_eq!(value.expose(), "token-123");

        let error = resolver
            .resolve(
                &SecretRef {
                    namespace: "auth_grant".to_owned(),
                    id: "missing".to_owned(),
                },
                None,
            )
            .await
            .expect_err("unknown ref must fail");
        assert!(matches!(error, SecretResolveError::NotFound { .. }));
    }

    #[tokio::test]
    async fn env_resolver_rejects_other_namespaces() {
        let resolver = EnvSecretResolver;

        let error = resolver
            .resolve(
                &SecretRef {
                    namespace: "auth_grant".to_owned(),
                    id: "g1".to_owned(),
                },
                None,
            )
            .await
            .expect_err("non-env namespace must fail");

        assert!(matches!(
            error,
            SecretResolveError::UnsupportedNamespace { .. }
        ));
    }
}
