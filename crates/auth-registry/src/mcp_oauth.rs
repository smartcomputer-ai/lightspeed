//! MCP OAuth driver (P69 G4): protected resource metadata discovery
//! (RFC 9728), authorization server metadata discovery (RFC 8414 / OIDC),
//! and client identification via Client ID Metadata Documents or dynamic
//! client registration (RFC 7591), with manual client records as the
//! fallback.
//!
//! The driver lazily upserts an [`OAuthClientRecord`] under the
//! `mcp:<server_id>` id convention; MCP catalog registration (P68) never
//! creates auth records. This crate stays MCP-catalog-agnostic: callers
//! describe the server with [`McpOAuthTarget`].

use async_trait::async_trait;
use thiserror::Error;

use crate::{
    AuthProviderKind, AuthRegistryError, CreateOAuthClientRecord, OAuthClientId,
    OAuthClientRecord, OAuthClientStore, PutSecretRecord, SECRET_KIND_OAUTH_CLIENT_SECRET,
    SecretId, SecretStore, SecretValue, TokenEndpointAuthMethod, random_auth_id,
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum McpOAuthError {
    #[error("metadata request to {url} failed{}: {message}", .status.map(|status| format!(" with status {status}")).unwrap_or_default())]
    Http {
        url: String,
        status: Option<u16>,
        message: String,
    },

    #[error("invalid metadata from {url}: {message}")]
    InvalidMetadata { url: String, message: String },

    #[error("no protected resource metadata found for {resource}: {detail}")]
    ProtectedResourceMetadataUnavailable { resource: String, detail: String },

    #[error("no authorization server metadata found for {issuer}: {detail}")]
    AuthorizationServerMetadataUnavailable { issuer: String, detail: String },

    #[error("protected resource metadata for {resource} lists no authorization servers")]
    NoAuthorizationServers { resource: String },

    #[error("authorization server {issuer} does not support PKCE S256")]
    PkceUnsupported { issuer: String },

    #[error(
        "authorization server {issuer} offers no usable client identification \
         (no CIMD support, no registration endpoint); register a client manually \
         with `forge auth client add --id {client_id}`"
    )]
    NoClientIdentification { issuer: String, client_id: String },

    #[error("dynamic client registration at {url} was rejected: {message}")]
    RegistrationRejected { url: String, message: String },

    #[error(transparent)]
    Registry(AuthRegistryError),
}

/// Transport for well-known metadata documents and dynamic client
/// registration. Mocked in tests; the real implementation is
/// [`HttpOAuthMetadataClient`].
#[async_trait]
pub trait OAuthMetadataClient: Send + Sync {
    async fn get_json(&self, url: &str) -> Result<serde_json::Value, McpOAuthError>;

    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, McpOAuthError>;
}

/// Protected resource metadata (RFC 9728), reduced to the fields the driver
/// needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedResourceMetadata {
    /// Canonical resource identifier; becomes the grant audience.
    pub resource: String,
    pub authorization_servers: Vec<String>,
}

/// Authorization server metadata (RFC 8414 / OIDC discovery), reduced to the
/// fields the driver needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub code_challenge_methods_supported: Option<Vec<String>>,
    /// 2025-11-25 MCP auth revision: the AS accepts HTTPS client-metadata
    /// URLs as client ids (draft-ietf-oauth-client-id-metadata-document).
    pub client_id_metadata_document_supported: bool,
}

/// What the caller knows about an OAuth-protected MCP server (from the P68
/// catalog record); the driver discovers the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOAuthTarget {
    pub server_id: String,
    pub server_url: String,
    pub scopes_default: Vec<String>,
    /// Explicit PRM URL from the catalog, tried before derived locations.
    pub protected_resource_metadata_url: Option<String>,
    /// Preferred authorization server when the PRM lists several.
    pub authorization_server_hint: Option<String>,
}

/// Client ID Metadata Document configuration: the public HTTPS URL where
/// this deployment serves its client metadata. The URL itself is the OAuth
/// client id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CimdConfig {
    pub client_id_url: String,
}

/// `mcp:<server_id>` — the id convention for lazily upserted MCP OAuth
/// clients and their grants' provider id.
pub fn mcp_oauth_client_id(server_id: &str) -> Result<OAuthClientId, AuthRegistryError> {
    OAuthClientId::try_new(format!("mcp:{server_id}")).map_err(|error| {
        AuthRegistryError::InvalidInput {
            message: format!("invalid mcp oauth client id: {error}"),
        }
    })
}

fn split_http_url(url: &str) -> Result<(String, String), McpOAuthError> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(McpOAuthError::InvalidMetadata {
            url: url.to_owned(),
            message: "URL has no scheme".to_owned(),
        });
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], rest[index..].trim_end_matches('/')),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(McpOAuthError::InvalidMetadata {
            url: url.to_owned(),
            message: "URL has no host".to_owned(),
        });
    }
    Ok((
        format!("{}://{authority}", scheme.to_ascii_lowercase()),
        path.to_owned(),
    ))
}

/// Candidate protected-resource-metadata URLs for a resource (RFC 9728 §3.1
/// path insertion, with a root fallback for servers that only serve the
/// host-wide document).
pub fn protected_resource_metadata_urls(resource: &str) -> Result<Vec<String>, McpOAuthError> {
    let (origin, path) = split_http_url(resource)?;
    let mut urls = vec![format!("{origin}/.well-known/oauth-protected-resource{path}")];
    if !path.is_empty() {
        urls.push(format!("{origin}/.well-known/oauth-protected-resource"));
    }
    Ok(urls)
}

/// Candidate authorization-server-metadata URLs for an issuer: RFC 8414 path
/// insertion for OAuth and OIDC documents, plus the OIDC path-appended form.
pub fn authorization_server_metadata_urls(issuer: &str) -> Result<Vec<String>, McpOAuthError> {
    let (origin, path) = split_http_url(issuer)?;
    let mut urls = vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}/.well-known/openid-configuration{path}"),
    ];
    if !path.is_empty() {
        urls.push(format!("{origin}{path}/.well-known/openid-configuration"));
    }
    urls.dedup();
    Ok(urls)
}

fn normalized(url: &str) -> &str {
    url.trim_end_matches('/')
}

fn parse_protected_resource_metadata(
    url: &str,
    value: &serde_json::Value,
    expected_resource: &str,
) -> Result<ProtectedResourceMetadata, McpOAuthError> {
    let invalid = |message: String| McpOAuthError::InvalidMetadata {
        url: url.to_owned(),
        message,
    };
    let Some(resource) = value.get("resource").and_then(|value| value.as_str()) else {
        return Err(invalid("missing resource".to_owned()));
    };
    if normalized(resource) != normalized(expected_resource) {
        return Err(invalid(format!(
            "metadata resource {resource} does not match {expected_resource}"
        )));
    }
    let authorization_servers: Vec<String> = value
        .get("authorization_servers")
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str())
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if authorization_servers.is_empty() {
        return Err(McpOAuthError::NoAuthorizationServers {
            resource: expected_resource.to_owned(),
        });
    }
    Ok(ProtectedResourceMetadata {
        resource: resource.to_owned(),
        authorization_servers,
    })
}

fn parse_authorization_server_metadata(
    url: &str,
    value: &serde_json::Value,
    expected_issuer: &str,
) -> Result<AuthorizationServerMetadata, McpOAuthError> {
    let invalid = |message: String| McpOAuthError::InvalidMetadata {
        url: url.to_owned(),
        message,
    };
    let require_str = |field: &str| {
        value
            .get(field)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("missing {field}")))
    };
    let issuer = require_str("issuer")?;
    if normalized(&issuer) != normalized(expected_issuer) {
        return Err(invalid(format!(
            "metadata issuer {issuer} does not match {expected_issuer}"
        )));
    }
    Ok(AuthorizationServerMetadata {
        issuer,
        authorization_endpoint: require_str("authorization_endpoint")?,
        token_endpoint: require_str("token_endpoint")?,
        registration_endpoint: value
            .get("registration_endpoint")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        code_challenge_methods_supported: value
            .get("code_challenge_methods_supported")
            .and_then(|value| value.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_str())
                    .map(str::to_owned)
                    .collect()
            }),
        client_id_metadata_document_supported: value
            .get("client_id_metadata_document_supported")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    })
}

/// Pick the authorization server to use: the catalog hint when the PRM lists
/// it, otherwise the first listed server.
pub fn select_authorization_server(
    metadata: &ProtectedResourceMetadata,
    hint: Option<&str>,
) -> String {
    if let Some(hint) = hint {
        if let Some(matched) = metadata
            .authorization_servers
            .iter()
            .find(|issuer| normalized(issuer) == normalized(hint))
        {
            return matched.clone();
        }
    }
    metadata.authorization_servers[0].clone()
}

struct ClientIdentification {
    remote_client_id: String,
    client_secret: Option<SecretValue>,
    auth_method: TokenEndpointAuthMethod,
}

/// Lazily discovers and registers OAuth clients for MCP servers.
pub struct McpOAuthDriver {
    clients: std::sync::Arc<dyn OAuthClientStore>,
    secrets: std::sync::Arc<dyn SecretStore>,
    metadata: std::sync::Arc<dyn OAuthMetadataClient>,
    now_ms: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl McpOAuthDriver {
    pub fn new(
        clients: std::sync::Arc<dyn OAuthClientStore>,
        secrets: std::sync::Arc<dyn SecretStore>,
        metadata: std::sync::Arc<dyn OAuthMetadataClient>,
    ) -> Self {
        Self {
            clients,
            secrets,
            metadata,
            now_ms: std::sync::Arc::new(crate::broker::system_now_ms),
        }
    }

    pub fn with_now_fn(
        mut self,
        now_ms: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        self.now_ms = now_ms;
        self
    }

    /// Return the `mcp:<server_id>` client record, discovering and creating
    /// it on first use. Existing records are reused without any network
    /// traffic; delete the client (`forge auth client remove`) to force
    /// re-discovery.
    pub async fn ensure_client(
        &self,
        target: &McpOAuthTarget,
        redirect_uri: &str,
        cimd: Option<&CimdConfig>,
    ) -> Result<OAuthClientRecord, McpOAuthError> {
        let client_id = mcp_oauth_client_id(&target.server_id).map_err(McpOAuthError::Registry)?;
        match self.clients.read_oauth_client(&client_id).await {
            Ok(existing) => return Ok(existing),
            Err(AuthRegistryError::ClientNotFound { .. }) => {}
            Err(error) => return Err(McpOAuthError::Registry(error)),
        }

        let prm = self.discover_protected_resource(target).await?;
        let issuer = select_authorization_server(&prm, target.authorization_server_hint.as_deref());
        let as_metadata = self.discover_authorization_server(&issuer).await?;
        if let Some(methods) = &as_metadata.code_challenge_methods_supported {
            if !methods.iter().any(|method| method == "S256") {
                return Err(McpOAuthError::PkceUnsupported {
                    issuer: as_metadata.issuer,
                });
            }
        }

        let identification = match (cimd, as_metadata.client_id_metadata_document_supported) {
            (Some(cimd), true) => ClientIdentification {
                remote_client_id: cimd.client_id_url.clone(),
                client_secret: None,
                auth_method: TokenEndpointAuthMethod::None,
            },
            _ => match &as_metadata.registration_endpoint {
                Some(registration_endpoint) => {
                    self.register_client(registration_endpoint, redirect_uri, &target.scopes_default)
                        .await?
                }
                None => {
                    return Err(McpOAuthError::NoClientIdentification {
                        issuer: as_metadata.issuer,
                        client_id: client_id.as_str().to_owned(),
                    });
                }
            },
        };

        let now_ms = (self.now_ms)();
        let client_secret_id = match identification.client_secret {
            Some(client_secret) => {
                let secret_id = SecretId::try_new(random_auth_id("authsec_")).map_err(|error| {
                    McpOAuthError::Registry(AuthRegistryError::Store {
                        message: format!("generate secret id: {error}"),
                    })
                })?;
                self.secrets
                    .put_secret(PutSecretRecord {
                        secret_id: secret_id.clone(),
                        secret_kind: SECRET_KIND_OAUTH_CLIENT_SECRET.to_owned(),
                        value: client_secret,
                        created_at_ms: now_ms,
                    })
                    .await
                    .map_err(McpOAuthError::Registry)?;
                Some(secret_id)
            }
            None => None,
        };

        let create = CreateOAuthClientRecord {
            client_id: client_id.clone(),
            provider_id: client_id.as_str().to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            display_name: None,
            authorization_endpoint: as_metadata.authorization_endpoint,
            token_endpoint: as_metadata.token_endpoint,
            remote_client_id: identification.remote_client_id,
            client_secret: client_secret_id.clone(),
            token_endpoint_auth_method: identification.auth_method,
            scopes_default: target.scopes_default.clone(),
            audience: Some(prm.resource),
            created_at_ms: now_ms,
        };
        match self.clients.create_oauth_client(create).await {
            Ok(record) => Ok(record),
            Err(AuthRegistryError::ClientAlreadyExists { .. }) => {
                // A concurrent login won the upsert race; discard our secret
                // and use the stored record.
                if let Some(secret_id) = &client_secret_id {
                    let _ = self.secrets.delete_secret(secret_id).await;
                }
                self.clients
                    .read_oauth_client(&client_id)
                    .await
                    .map_err(McpOAuthError::Registry)
            }
            Err(error) => {
                if let Some(secret_id) = &client_secret_id {
                    let _ = self.secrets.delete_secret(secret_id).await;
                }
                Err(McpOAuthError::Registry(error))
            }
        }
    }

    async fn discover_protected_resource(
        &self,
        target: &McpOAuthTarget,
    ) -> Result<ProtectedResourceMetadata, McpOAuthError> {
        let mut candidates = Vec::new();
        if let Some(explicit) = &target.protected_resource_metadata_url {
            candidates.push(explicit.clone());
        }
        candidates.extend(protected_resource_metadata_urls(&target.server_url)?);
        candidates.dedup();

        let mut last_error = String::new();
        for url in &candidates {
            match self.metadata.get_json(url).await {
                Ok(value) => {
                    match parse_protected_resource_metadata(url, &value, &target.server_url) {
                        Ok(metadata) => return Ok(metadata),
                        Err(error @ McpOAuthError::NoAuthorizationServers { .. }) => {
                            return Err(error);
                        }
                        Err(error) => last_error = error.to_string(),
                    }
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        Err(McpOAuthError::ProtectedResourceMetadataUnavailable {
            resource: target.server_url.clone(),
            detail: last_error,
        })
    }

    async fn discover_authorization_server(
        &self,
        issuer: &str,
    ) -> Result<AuthorizationServerMetadata, McpOAuthError> {
        let candidates = authorization_server_metadata_urls(issuer)?;
        let mut last_error = String::new();
        for url in &candidates {
            match self.metadata.get_json(url).await {
                Ok(value) => match parse_authorization_server_metadata(url, &value, issuer) {
                    Ok(metadata) => return Ok(metadata),
                    Err(error) => last_error = error.to_string(),
                },
                Err(error) => last_error = error.to_string(),
            }
        }
        Err(McpOAuthError::AuthorizationServerMetadataUnavailable {
            issuer: issuer.to_owned(),
            detail: last_error,
        })
    }

    async fn register_client(
        &self,
        registration_endpoint: &str,
        redirect_uri: &str,
        scopes: &[String],
    ) -> Result<ClientIdentification, McpOAuthError> {
        let mut body = serde_json::json!({
            "client_name": "Forge",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        if !scopes.is_empty() {
            body["scope"] = serde_json::Value::String(scopes.join(" "));
        }
        let response = self.metadata.post_json(registration_endpoint, &body).await?;
        let invalid = |message: String| McpOAuthError::InvalidMetadata {
            url: registration_endpoint.to_owned(),
            message,
        };
        let Some(remote_client_id) = response
            .get("client_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
        else {
            return Err(invalid(
                "registration response is missing client_id".to_owned(),
            ));
        };
        let client_secret = response
            .get("client_secret")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(SecretValue::new);
        let auth_method = match response
            .get("token_endpoint_auth_method")
            .and_then(|value| value.as_str())
        {
            Some("client_secret_basic") => TokenEndpointAuthMethod::ClientSecretBasic,
            Some("client_secret_post") => TokenEndpointAuthMethod::ClientSecretPost,
            Some("none") => TokenEndpointAuthMethod::None,
            _ => {
                if client_secret.is_some() {
                    TokenEndpointAuthMethod::ClientSecretBasic
                } else {
                    TokenEndpointAuthMethod::None
                }
            }
        };
        match (auth_method, client_secret) {
            (TokenEndpointAuthMethod::None, _) => Ok(ClientIdentification {
                remote_client_id: remote_client_id.to_owned(),
                // A secret issued alongside method "none" is unusable.
                client_secret: None,
                auth_method: TokenEndpointAuthMethod::None,
            }),
            (method, Some(client_secret)) => Ok(ClientIdentification {
                remote_client_id: remote_client_id.to_owned(),
                client_secret: Some(client_secret),
                auth_method: method,
            }),
            (_, None) => Err(invalid(
                "registration requires a client secret auth method but issued no client_secret"
                    .to_owned(),
            )),
        }
    }
}

/// Real metadata transport over HTTPS: bounded redirects, JSON-only, and
/// RFC 7591 error bodies surfaced as [`McpOAuthError::RegistrationRejected`].
pub struct HttpOAuthMetadataClient {
    http: reqwest::Client,
}

impl HttpOAuthMetadataClient {
    pub fn new() -> Result<Self, McpOAuthError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|error| McpOAuthError::Http {
                url: String::new(),
                status: None,
                message: format!("build http client: {error}"),
            })?;
        Ok(Self { http })
    }
}

#[async_trait]
impl OAuthMetadataClient for HttpOAuthMetadataClient {
    async fn get_json(&self, url: &str) -> Result<serde_json::Value, McpOAuthError> {
        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| McpOAuthError::Http {
                url: url.to_owned(),
                status: error.status().map(|status| status.as_u16()),
                message: format!("metadata request failed: {error}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(McpOAuthError::Http {
                url: url.to_owned(),
                status: Some(status.as_u16()),
                message: "metadata request returned a non-success status".to_owned(),
            });
        }
        response
            .json()
            .await
            .map_err(|_| McpOAuthError::InvalidMetadata {
                url: url.to_owned(),
                message: "metadata response is not valid JSON".to_owned(),
            })
    }

    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, McpOAuthError> {
        let response = self
            .http
            .post(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(body)
            .send()
            .await
            .map_err(|error| McpOAuthError::Http {
                url: url.to_owned(),
                status: error.status().map(|status| status.as_u16()),
                message: format!("registration request failed: {error}"),
            })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| McpOAuthError::Http {
            url: url.to_owned(),
            status: Some(status.as_u16()),
            message: format!("read registration response: {error}"),
        })?;
        if !status.is_success() {
            // RFC 7591 §3.2.2 error response; never echo unparsed bodies.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
                    let description = value
                        .get("error_description")
                        .and_then(|description| description.as_str())
                        .unwrap_or_default();
                    return Err(McpOAuthError::RegistrationRejected {
                        url: url.to_owned(),
                        message: if description.is_empty() {
                            error.to_owned()
                        } else {
                            format!("{error}: {description}")
                        },
                    });
                }
            }
            return Err(McpOAuthError::Http {
                url: url.to_owned(),
                status: Some(status.as_u16()),
                message: "registration request returned a non-success status".to_owned(),
            });
        }
        serde_json::from_str(&text).map_err(|_| McpOAuthError::InvalidMetadata {
            url: url.to_owned(),
            message: "registration response is not valid JSON".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{InMemoryOAuthClientStore, InMemorySecretStore};

    #[test]
    fn protected_resource_metadata_urls_insert_path_with_root_fallback() {
        assert_eq!(
            protected_resource_metadata_urls("https://host.example.com/mcp-auth-server")
                .expect("urls"),
            vec![
                "https://host.example.com/.well-known/oauth-protected-resource/mcp-auth-server"
                    .to_owned(),
                "https://host.example.com/.well-known/oauth-protected-resource".to_owned(),
            ]
        );
        assert_eq!(
            protected_resource_metadata_urls("https://host.example.com").expect("urls"),
            vec!["https://host.example.com/.well-known/oauth-protected-resource".to_owned()]
        );
    }

    #[test]
    fn authorization_server_metadata_urls_cover_oauth_and_oidc_forms() {
        assert_eq!(
            authorization_server_metadata_urls("https://as.example.com").expect("urls"),
            vec![
                "https://as.example.com/.well-known/oauth-authorization-server".to_owned(),
                "https://as.example.com/.well-known/openid-configuration".to_owned(),
            ]
        );
        assert_eq!(
            authorization_server_metadata_urls("https://as.example.com/tenant1").expect("urls"),
            vec![
                "https://as.example.com/.well-known/oauth-authorization-server/tenant1".to_owned(),
                "https://as.example.com/.well-known/openid-configuration/tenant1".to_owned(),
                "https://as.example.com/tenant1/.well-known/openid-configuration".to_owned(),
            ]
        );
    }

    #[test]
    fn authorization_server_selection_prefers_a_listed_hint() {
        let metadata = ProtectedResourceMetadata {
            resource: "https://crm.example.com/mcp".to_owned(),
            authorization_servers: vec![
                "https://as-one.example.com".to_owned(),
                "https://as-two.example.com".to_owned(),
            ],
        };

        assert_eq!(
            select_authorization_server(&metadata, Some("https://as-two.example.com/")),
            "https://as-two.example.com"
        );
        assert_eq!(
            select_authorization_server(&metadata, Some("https://unlisted.example.com")),
            "https://as-one.example.com"
        );
        assert_eq!(
            select_authorization_server(&metadata, None),
            "https://as-one.example.com"
        );
    }

    struct MockMetadataClient {
        gets: BTreeMap<String, serde_json::Value>,
        post_response: Option<Result<serde_json::Value, McpOAuthError>>,
        posts: Mutex<Vec<(String, serde_json::Value)>>,
        get_calls: Mutex<Vec<String>>,
    }

    impl MockMetadataClient {
        fn new(gets: Vec<(&str, serde_json::Value)>) -> Self {
            Self {
                gets: gets
                    .into_iter()
                    .map(|(url, value)| (url.to_owned(), value))
                    .collect(),
                post_response: None,
                posts: Mutex::new(Vec::new()),
                get_calls: Mutex::new(Vec::new()),
            }
        }

        fn with_post_response(
            mut self,
            response: Result<serde_json::Value, McpOAuthError>,
        ) -> Self {
            self.post_response = Some(response);
            self
        }
    }

    #[async_trait]
    impl OAuthMetadataClient for MockMetadataClient {
        async fn get_json(&self, url: &str) -> Result<serde_json::Value, McpOAuthError> {
            self.get_calls.lock().expect("lock").push(url.to_owned());
            self.gets
                .get(url)
                .cloned()
                .ok_or_else(|| McpOAuthError::Http {
                    url: url.to_owned(),
                    status: Some(404),
                    message: "not found".to_owned(),
                })
        }

        async fn post_json(
            &self,
            url: &str,
            body: &serde_json::Value,
        ) -> Result<serde_json::Value, McpOAuthError> {
            self.posts
                .lock()
                .expect("lock")
                .push((url.to_owned(), body.clone()));
            self.post_response
                .clone()
                .unwrap_or_else(|| {
                    Err(McpOAuthError::Http {
                        url: url.to_owned(),
                        status: Some(404),
                        message: "no post response scripted".to_owned(),
                    })
                })
        }
    }

    const RESOURCE: &str = "https://crm.example.com/mcp";
    const PRM_URL: &str = "https://crm.example.com/.well-known/oauth-protected-resource/mcp";
    const AS_URL: &str = "https://as.example.com/.well-known/oauth-authorization-server";

    fn prm_doc() -> serde_json::Value {
        serde_json::json!({
            "resource": RESOURCE,
            "authorization_servers": ["https://as.example.com"],
        })
    }

    fn as_doc(extra: serde_json::Value) -> serde_json::Value {
        let mut doc = serde_json::json!({
            "issuer": "https://as.example.com",
            "authorization_endpoint": "https://as.example.com/authorize",
            "token_endpoint": "https://as.example.com/token",
            "code_challenge_methods_supported": ["S256"],
        });
        if let (Some(doc), Some(extra)) = (doc.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                doc.insert(key.clone(), value.clone());
            }
        }
        doc
    }

    fn target() -> McpOAuthTarget {
        McpOAuthTarget {
            server_id: "playground".to_owned(),
            server_url: RESOURCE.to_owned(),
            scopes_default: vec!["contacts.read".to_owned()],
            protected_resource_metadata_url: None,
            authorization_server_hint: None,
        }
    }

    struct Harness {
        driver: McpOAuthDriver,
        clients: Arc<InMemoryOAuthClientStore>,
        secrets: Arc<InMemorySecretStore>,
        metadata: Arc<MockMetadataClient>,
    }

    fn harness(metadata: MockMetadataClient) -> Harness {
        let clients = Arc::new(InMemoryOAuthClientStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let metadata = Arc::new(metadata);
        let driver = McpOAuthDriver::new(clients.clone(), secrets.clone(), metadata.clone())
            .with_now_fn(Arc::new(|| 1_000));
        Harness {
            driver,
            clients,
            secrets,
            metadata,
        }
    }

    const REDIRECT: &str = "https://forge.example.com/auth/callback";

    #[tokio::test]
    async fn ensure_client_discovers_and_registers_via_dcr() {
        let harness = harness(
            MockMetadataClient::new(vec![
                (PRM_URL, prm_doc()),
                (
                    AS_URL,
                    as_doc(serde_json::json!({
                        "registration_endpoint": "https://as.example.com/register",
                    })),
                ),
            ])
            .with_post_response(Ok(serde_json::json!({
                "client_id": "dcr-client-1",
                "token_endpoint_auth_method": "none",
            }))),
        );

        let record = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect("ensure client");

        assert_eq!(record.client_id, OAuthClientId::new("mcp:playground"));
        assert_eq!(record.provider_id, "mcp:playground");
        assert_eq!(record.provider_kind, AuthProviderKind::McpOAuth);
        assert_eq!(record.remote_client_id, "dcr-client-1");
        assert_eq!(record.token_endpoint_auth_method, TokenEndpointAuthMethod::None);
        assert_eq!(record.audience.as_deref(), Some(RESOURCE));
        assert_eq!(
            record.authorization_endpoint,
            "https://as.example.com/authorize"
        );
        assert!(record.client_secret.is_none());

        // The DCR request asked for a public PKCE client with our redirect.
        let posts = harness.metadata.posts.lock().expect("lock");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "https://as.example.com/register");
        assert_eq!(posts[0].1["redirect_uris"][0], REDIRECT);
        assert_eq!(posts[0].1["token_endpoint_auth_method"], "none");
        assert_eq!(posts[0].1["scope"], "contacts.read");
    }

    #[tokio::test]
    async fn ensure_client_stores_dcr_issued_secrets_encrypted() {
        let harness = harness(
            MockMetadataClient::new(vec![
                (PRM_URL, prm_doc()),
                (
                    AS_URL,
                    as_doc(serde_json::json!({
                        "registration_endpoint": "https://as.example.com/register",
                    })),
                ),
            ])
            .with_post_response(Ok(serde_json::json!({
                "client_id": "dcr-client-2",
                "client_secret": "dcr-secret",
                "token_endpoint_auth_method": "client_secret_basic",
            }))),
        );

        let record = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect("ensure client");

        assert_eq!(
            record.token_endpoint_auth_method,
            TokenEndpointAuthMethod::ClientSecretBasic
        );
        let secret_id = record.client_secret.expect("client secret stored");
        let (meta, value) = harness
            .secrets
            .read_secret(&secret_id)
            .await
            .expect("read client secret");
        assert_eq!(meta.secret_kind, SECRET_KIND_OAUTH_CLIENT_SECRET);
        assert_eq!(value.expose(), "dcr-secret");
    }

    #[tokio::test]
    async fn ensure_client_prefers_cimd_when_the_as_supports_it() {
        let harness = harness(MockMetadataClient::new(vec![
            (PRM_URL, prm_doc()),
            (
                AS_URL,
                as_doc(serde_json::json!({
                    "registration_endpoint": "https://as.example.com/register",
                    "client_id_metadata_document_supported": true,
                })),
            ),
        ]));
        let cimd = CimdConfig {
            client_id_url: "https://forge.example.com/auth/client-metadata.json".to_owned(),
        };

        let record = harness
            .driver
            .ensure_client(&target(), REDIRECT, Some(&cimd))
            .await
            .expect("ensure client");

        assert_eq!(record.remote_client_id, cimd.client_id_url);
        assert_eq!(record.token_endpoint_auth_method, TokenEndpointAuthMethod::None);
        // No registration request was made.
        assert!(harness.metadata.posts.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn ensure_client_falls_back_to_dcr_when_cimd_is_unsupported() {
        let harness = harness(
            MockMetadataClient::new(vec![
                (PRM_URL, prm_doc()),
                (
                    AS_URL,
                    as_doc(serde_json::json!({
                        "registration_endpoint": "https://as.example.com/register",
                    })),
                ),
            ])
            .with_post_response(Ok(serde_json::json!({"client_id": "dcr-client-3"}))),
        );
        let cimd = CimdConfig {
            client_id_url: "https://forge.example.com/auth/client-metadata.json".to_owned(),
        };

        let record = harness
            .driver
            .ensure_client(&target(), REDIRECT, Some(&cimd))
            .await
            .expect("ensure client");

        assert_eq!(record.remote_client_id, "dcr-client-3");
    }

    #[tokio::test]
    async fn ensure_client_reuses_existing_records_without_network() {
        let harness = harness(MockMetadataClient::new(Vec::new()));
        harness
            .clients
            .create_oauth_client(CreateOAuthClientRecord {
                client_id: OAuthClientId::new("mcp:playground"),
                provider_id: "mcp:playground".to_owned(),
                provider_kind: AuthProviderKind::McpOAuth,
                display_name: None,
                authorization_endpoint: "https://as.example.com/authorize".to_owned(),
                token_endpoint: "https://as.example.com/token".to_owned(),
                remote_client_id: "manual-client".to_owned(),
                client_secret: None,
                token_endpoint_auth_method: TokenEndpointAuthMethod::None,
                scopes_default: Vec::new(),
                audience: Some(RESOURCE.to_owned()),
                created_at_ms: 10,
            })
            .await
            .expect("create existing client");

        let record = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect("ensure client");

        assert_eq!(record.remote_client_id, "manual-client");
        assert!(harness.metadata.get_calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn ensure_client_uses_the_root_prm_fallback() {
        let harness = harness(
            MockMetadataClient::new(vec![
                (
                    "https://crm.example.com/.well-known/oauth-protected-resource",
                    prm_doc(),
                ),
                (
                    AS_URL,
                    as_doc(serde_json::json!({
                        "registration_endpoint": "https://as.example.com/register",
                    })),
                ),
            ])
            .with_post_response(Ok(serde_json::json!({"client_id": "dcr-client-4"}))),
        );

        harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect("ensure client via root PRM");

        let calls = harness.metadata.get_calls.lock().expect("lock");
        assert!(calls.contains(&PRM_URL.to_owned()), "path form tried first");
    }

    #[tokio::test]
    async fn ensure_client_rejects_prm_resource_mismatch() {
        let harness = harness(MockMetadataClient::new(vec![(
            PRM_URL,
            serde_json::json!({
                "resource": "https://evil.example.com/mcp",
                "authorization_servers": ["https://as.example.com"],
            }),
        )]));

        let error = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect_err("resource mismatch must fail");

        assert!(matches!(
            error,
            McpOAuthError::ProtectedResourceMetadataUnavailable { .. }
        ));
    }

    #[tokio::test]
    async fn ensure_client_rejects_as_without_s256() {
        let harness = harness(MockMetadataClient::new(vec![
            (PRM_URL, prm_doc()),
            (
                AS_URL,
                serde_json::json!({
                    "issuer": "https://as.example.com",
                    "authorization_endpoint": "https://as.example.com/authorize",
                    "token_endpoint": "https://as.example.com/token",
                    "code_challenge_methods_supported": ["plain"],
                    "registration_endpoint": "https://as.example.com/register",
                }),
            ),
        ]));

        let error = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect_err("missing S256 must fail");

        assert!(matches!(error, McpOAuthError::PkceUnsupported { .. }));
    }

    #[tokio::test]
    async fn ensure_client_requires_some_client_identification() {
        let harness = harness(MockMetadataClient::new(vec![
            (PRM_URL, prm_doc()),
            (AS_URL, as_doc(serde_json::json!({}))),
        ]));

        let error = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect_err("no CIMD and no DCR must fail");

        assert!(matches!(
            error,
            McpOAuthError::NoClientIdentification { .. }
        ));
    }

    #[tokio::test]
    async fn ensure_client_rejects_issuer_mismatch() {
        let harness = harness(MockMetadataClient::new(vec![
            (PRM_URL, prm_doc()),
            (
                AS_URL,
                serde_json::json!({
                    "issuer": "https://other-as.example.com",
                    "authorization_endpoint": "https://as.example.com/authorize",
                    "token_endpoint": "https://as.example.com/token",
                }),
            ),
        ]));

        let error = harness
            .driver
            .ensure_client(&target(), REDIRECT, None)
            .await
            .expect_err("issuer mismatch must fail");

        assert!(matches!(
            error,
            McpOAuthError::AuthorizationServerMetadataUnavailable { .. }
        ));
    }
}
