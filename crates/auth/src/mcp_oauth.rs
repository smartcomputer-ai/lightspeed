//! MCP OAuth protocol adapter.
//!
//! `rmcp` owns MCP discovery, registration, PKCE, issuer/resource binding,
//! challenges, token exchange, and refresh wire behavior. Lightspeed keeps
//! durable clients, one-time flows, encrypted secrets, grants, and broker
//! authority.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oauth2::{CsrfToken, PkceCodeVerifier};
use rmcp::transport::auth::{
    AuthError as RmcpAuthError, AuthorizationManager, AuthorizationMetadata,
    AuthorizationMetadataResolution, AuthorizationMetadataSource, AuthorizationRequest,
    AuthorizationSession, CredentialStore, InMemoryCredentialStore, OAuthClientConfig,
    OAuthHttpClient, OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRequest, StateStore,
    StoredAuthorizationState, StoredCredentials, WWWAuthenticateParams,
};
use thiserror::Error;

use crate::{
    AuthProviderKind, AuthRegistryError, CreateOAuthClientRecord, OAuthClientId, OAuthClientRecord,
    OAuthClientStore, OAuthTokenError, OAuthTokenGrant, OAuthTokenRequest, OAuthTokenResponse,
    PinnedHttpPolicy, PrivateNetworkList, PutSecretRecord, SECRET_KIND_OAUTH_CLIENT_SECRET,
    SecretId, SecretStore, SecretValue, TokenEndpointAuthMethod, random_auth_id,
};

pub use rmcp::transport::auth::OAuthHttpClient as OAuthMetadataClient;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum McpOAuthError {
    #[error("MCP OAuth metadata discovery failed for {resource}: {message}")]
    Discovery { resource: String, message: String },

    #[error("no protected resource metadata found for {resource}: {detail}")]
    ProtectedResourceMetadataUnavailable { resource: String, detail: String },

    #[error("authorization server {issuer} does not support PKCE S256")]
    PkceUnsupported { issuer: String },

    #[error(
        "authorization server {issuer} offers no usable client identification; register a client manually with id {client_id}"
    )]
    NoClientIdentification { issuer: String, client_id: String },

    #[error("MCP OAuth client registration failed: {message}")]
    RegistrationRejected { message: String },

    #[error("MCP OAuth callback issuer is invalid: {message}")]
    InvalidIssuer { message: String },

    #[error("MCP OAuth protocol operation failed: {message}")]
    Protocol { message: String },

    #[error(transparent)]
    Registry(AuthRegistryError),
}

/// Public protected-resource facts used by the management discovery API.
/// `rmcp` selects one usable authorization server, so this view contains that
/// issuer rather than snapshotting an OAuth-server inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    pub authorization_servers: Vec<String>,
    pub scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOAuthTarget {
    pub server_id: String,
    pub server_url: String,
    pub scopes_default: Vec<String>,
    /// Explicit same-origin PRM URL. It is supplied to `rmcp` as the
    /// challenge's `resource_metadata` pointer.
    pub protected_resource_metadata_url: Option<String>,
    /// Optional expected issuer. The SDK selects the server from published
    /// metadata; this hint constrains that result rather than overriding it.
    pub authorization_server_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CimdConfig {
    pub client_id_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOAuthChallenge {
    pub resource_metadata_url: Option<String>,
    pub required_scopes: Vec<String>,
    pub insufficient_scope: bool,
    pub invalid_token: bool,
}

pub fn parse_mcp_oauth_challenge(
    header: &str,
    server_url: &str,
) -> Result<McpOAuthChallenge, McpOAuthError> {
    let base = reqwest::Url::parse(server_url).map_err(|_| McpOAuthError::Protocol {
        message: "MCP server URL is invalid".to_owned(),
    })?;
    let parsed = WWWAuthenticateParams::parse(header, &base);
    Ok(McpOAuthChallenge {
        resource_metadata_url: parsed
            .resource_metadata_url
            .as_ref()
            .map(|url| url.to_string()),
        required_scopes: parsed
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        insufficient_scope: parsed.is_insufficient_scope(),
        invalid_token: parsed.is_invalid_token(),
    })
}

pub fn mcp_oauth_client_id(server_id: &str) -> Result<OAuthClientId, AuthRegistryError> {
    OAuthClientId::try_new(format!("mcp:{server_id}")).map_err(|error| {
        AuthRegistryError::InvalidInput {
            message: format!("invalid mcp oauth client id: {error}"),
        }
    })
}

pub struct HttpOAuthMetadataClient {
    policy: PinnedHttpPolicy,
}

impl HttpOAuthMetadataClient {
    pub fn new() -> Result<Self, McpOAuthError> {
        Ok(Self {
            policy: PinnedHttpPolicy::public_only(),
        })
    }

    /// A client that reaches public HTTPS endpoints plus the deployment's
    /// listed private destinations. Metadata, registration, exchange and
    /// refresh each name their own URL, and each URL is judged on its own.
    pub fn with_private_networks(private_networks: PrivateNetworkList) -> Self {
        Self {
            policy: PinnedHttpPolicy::public_only().with_private_networks(private_networks),
        }
    }

    pub fn policy(&self) -> &PinnedHttpPolicy {
        &self.policy
    }
}

impl OAuthHttpClient for HttpOAuthMetadataClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        self.policy.execute(request)
    }
}

pub struct McpOAuthDriver {
    clients: Arc<dyn OAuthClientStore>,
    secrets: Arc<dyn SecretStore>,
    http: Arc<dyn OAuthMetadataClient>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl McpOAuthDriver {
    pub fn new(
        clients: Arc<dyn OAuthClientStore>,
        secrets: Arc<dyn SecretStore>,
        http: Arc<dyn OAuthMetadataClient>,
    ) -> Self {
        Self {
            clients,
            secrets,
            http,
            now_ms: Arc::new(crate::broker::system_now_ms),
        }
    }

    pub fn with_now_fn(mut self, now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.now_ms = now_ms;
        self
    }

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

        let (mut manager, resolution) = self.resolve(target).await?;
        let metadata = resolution.metadata;
        validate_pkce(&metadata)?;
        manager.set_metadata(metadata.clone());
        let discovered_scopes = manager.select_scopes(None, &[]);

        let supports_cimd = metadata
            .additional_fields
            .get("client_id_metadata_document_supported")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let (remote_client_id, client_secret, auth_method) = if supports_cimd {
            match cimd {
                Some(cimd) => (
                    cimd.client_id_url.clone(),
                    None,
                    TokenEndpointAuthMethod::None,
                ),
                None => {
                    self.register_client(&mut manager, &metadata, redirect_uri, target)
                        .await?
                }
            }
        } else {
            self.register_client(&mut manager, &metadata, redirect_uri, target)
                .await?
        };

        let now_ms = (self.now_ms)();
        let client_secret_id = match client_secret {
            Some(client_secret) => {
                let secret_id = random_secret_id()?;
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

        let issuer = metadata.issuer.clone();
        let require_issuer =
            metadata_bool(&metadata, "authorization_response_iss_parameter_supported");
        let create = CreateOAuthClientRecord {
            client_id: client_id.clone(),
            provider_id: client_id.as_str().to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            display_name: None,
            authorization_endpoint: metadata.authorization_endpoint,
            token_endpoint: metadata.token_endpoint,
            remote_client_id,
            client_secret: client_secret_id.clone(),
            token_endpoint_auth_method: auth_method,
            scopes_default: target.scopes_default.clone(),
            audience: Some(target.server_url.clone()),
            authorization_server_issuer: issuer,
            authorization_response_iss_parameter_supported: require_issuer,
            authorization_server_scopes_supported: discovered_scopes,
            created_at_ms: now_ms,
        };
        match self.clients.create_oauth_client(create).await {
            Ok(record) => Ok(record),
            Err(AuthRegistryError::ClientAlreadyExists { .. }) => {
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

    pub async fn discover_protected_resource(
        &self,
        target: &McpOAuthTarget,
    ) -> Result<ProtectedResourceMetadata, McpOAuthError> {
        let (mut manager, resolution) = self.resolve(target).await?;
        manager.set_metadata(resolution.metadata.clone());
        let scopes_supported = manager.select_scopes(None, &[]);
        let issuer = resolution.metadata.issuer.clone().ok_or_else(|| {
            McpOAuthError::ProtectedResourceMetadataUnavailable {
                resource: target.server_url.clone(),
                detail: "selected authorization server metadata has no issuer".to_owned(),
            }
        })?;
        Ok(ProtectedResourceMetadata {
            resource: target.server_url.clone(),
            authorization_servers: vec![issuer],
            scopes_supported,
        })
    }

    async fn resolve(
        &self,
        target: &McpOAuthTarget,
    ) -> Result<(AuthorizationManager, AuthorizationMetadataResolution), McpOAuthError> {
        let manager =
            AuthorizationManager::new_with_oauth_http_client(&target.server_url, self.http.clone())
                .await
                .map_err(|error| discovery_error(target, error))?;
        let resolution = if let Some(explicit) = &target.protected_resource_metadata_url {
            let challenge = format!("Bearer resource_metadata=\"{}\"", explicit);
            manager
                .resolve_metadata_from_challenge(Some(&challenge))
                .await
        } else {
            manager.resolve_metadata().await
        }
        .map_err(|error| discovery_error(target, error))?;

        if resolution.source != AuthorizationMetadataSource::ProtectedResourceMetadata {
            return Err(McpOAuthError::ProtectedResourceMetadataUnavailable {
                resource: target.server_url.clone(),
                detail: match resolution.source {
                    AuthorizationMetadataSource::LegacyEndpointFallback => {
                        "the endpoint published no current MCP OAuth metadata".to_owned()
                    }
                    _ => "the endpoint exposed authorization-server metadata but no protected-resource metadata".to_owned(),
                },
            });
        }
        if let Some(hint) = &target.authorization_server_hint {
            let selected = resolution.metadata.issuer.as_deref().unwrap_or_default();
            if normalize_url(selected) != normalize_url(hint) {
                return Err(McpOAuthError::Discovery {
                    resource: target.server_url.clone(),
                    message: format!(
                        "published metadata selected authorization server {selected}, not configured issuer {hint}"
                    ),
                });
            }
        }
        Ok((manager, resolution))
    }

    async fn register_client(
        &self,
        manager: &mut AuthorizationManager,
        metadata: &AuthorizationMetadata,
        redirect_uri: &str,
        target: &McpOAuthTarget,
    ) -> Result<(String, Option<SecretValue>, TokenEndpointAuthMethod), McpOAuthError> {
        if metadata.registration_endpoint.is_none() {
            return Err(McpOAuthError::NoClientIdentification {
                issuer: metadata.issuer.clone().unwrap_or_default(),
                client_id: mcp_oauth_client_id(&target.server_id)
                    .map_err(McpOAuthError::Registry)?
                    .to_string(),
            });
        }

        // `register_client` uses the manager's application type. Configure a
        // throwaway web client solely to select `application_type: web`; the
        // returned DCR config immediately replaces it.
        manager
            .configure_client(
                OAuthClientConfig::new("lightspeed-registration", redirect_uri)
                    .with_application_type("web"),
            )
            .map_err(protocol_error)?;
        let scopes: Vec<&str> = target.scopes_default.iter().map(String::as_str).collect();
        let config = manager
            .register_client("Lightspeed", redirect_uri, &scopes)
            .await
            .map_err(|error| McpOAuthError::RegistrationRejected {
                message: sanitize_rmcp_error(error),
            })?;
        let auth_method = token_auth_method(metadata, config.client_secret.is_some());
        Ok((
            config.client_id,
            config.client_secret.map(SecretValue::new),
            auth_method,
        ))
    }
}

fn validate_pkce(metadata: &AuthorizationMetadata) -> Result<(), McpOAuthError> {
    if metadata
        .code_challenge_methods_supported
        .as_ref()
        .is_some_and(|methods| !methods.iter().any(|method| method == "S256"))
    {
        return Err(McpOAuthError::PkceUnsupported {
            issuer: metadata.issuer.clone().unwrap_or_default(),
        });
    }
    Ok(())
}

fn token_auth_method(
    metadata: &AuthorizationMetadata,
    has_secret: bool,
) -> TokenEndpointAuthMethod {
    if !has_secret {
        return TokenEndpointAuthMethod::None;
    }
    let methods = metadata
        .additional_fields
        .get("token_endpoint_auth_methods_supported")
        .and_then(serde_json::Value::as_array);
    if methods.is_some_and(|methods| {
        let basic = methods
            .iter()
            .any(|method| method.as_str() == Some("client_secret_basic"));
        let post = methods
            .iter()
            .any(|method| method.as_str() == Some("client_secret_post"));
        post && !basic
    }) {
        TokenEndpointAuthMethod::ClientSecretPost
    } else {
        TokenEndpointAuthMethod::ClientSecretBasic
    }
}

fn metadata_bool(metadata: &AuthorizationMetadata, field: &str) -> bool {
    metadata
        .additional_fields
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn normalize_url(value: &str) -> &str {
    value.trim_end_matches('/')
}

fn discovery_error(target: &McpOAuthTarget, error: RmcpAuthError) -> McpOAuthError {
    match error {
        RmcpAuthError::PkceUnsupported => McpOAuthError::PkceUnsupported {
            issuer: target.authorization_server_hint.clone().unwrap_or_default(),
        },
        RmcpAuthError::AuthorizationServerMismatch { .. }
        | RmcpAuthError::AuthorizationServerMissingIssuer { .. } => McpOAuthError::InvalidIssuer {
            message: sanitize_rmcp_error(error),
        },
        other => McpOAuthError::ProtectedResourceMetadataUnavailable {
            resource: target.server_url.clone(),
            detail: sanitize_rmcp_error(other),
        },
    }
}

fn protocol_error(error: RmcpAuthError) -> McpOAuthError {
    match error {
        RmcpAuthError::AuthorizationServerMismatch { .. }
        | RmcpAuthError::AuthorizationServerMissingIssuer { .. } => McpOAuthError::InvalidIssuer {
            message: sanitize_rmcp_error(error),
        },
        other => McpOAuthError::Protocol {
            message: sanitize_rmcp_error(other),
        },
    }
}

fn sanitize_rmcp_error(error: RmcpAuthError) -> String {
    match error {
        RmcpAuthError::AuthorizationServerMismatch {
            expected_issuer,
            received_issuer,
        } => format!(
            "authorization server issuer mismatch: expected {expected_issuer}, received {received_issuer}"
        ),
        RmcpAuthError::AuthorizationServerMissingIssuer { expected_issuer } => {
            format!("authorization response is missing required issuer {expected_issuer}")
        }
        RmcpAuthError::PkceUnsupported => {
            "authorization server does not support PKCE S256".to_owned()
        }
        RmcpAuthError::NoAuthorizationSupport => {
            "server published no supported OAuth metadata".to_owned()
        }
        RmcpAuthError::RegistrationFailed(_) => "client registration was rejected".to_owned(),
        RmcpAuthError::TokenRefreshRejected(_) => "refresh token was rejected".to_owned(),
        RmcpAuthError::InsufficientScope { required_scope, .. } => {
            format!("additional consent is required for scope {required_scope}")
        }
        _ => "remote OAuth operation failed".to_owned(),
    }
}

fn random_secret_id() -> Result<SecretId, McpOAuthError> {
    SecretId::try_new(random_auth_id("authsec_")).map_err(|error| {
        McpOAuthError::Registry(AuthRegistryError::Store {
            message: format!("generate secret id: {error}"),
        })
    })
}

#[derive(Clone, Debug)]
pub struct McpOAuthTokenContext {
    pub authorization_endpoint: String,
    pub issuer: Option<String>,
    pub require_issuer: bool,
    pub scopes_supported: Vec<String>,
    pub requested_scopes: Vec<String>,
    pub callback_state: Option<SecretValue>,
    pub callback_issuer: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RmcpAuthorizationStart {
    pub authorize_url: String,
    pub state: String,
    pub pkce_verifier: SecretValue,
    pub expected_issuer: Option<String>,
    pub require_issuer: bool,
}

pub(crate) async fn begin_rmcp_authorization(
    client: &OAuthClientRecord,
    client_secret: Option<&SecretValue>,
    redirect_uri: &str,
    scopes: &[String],
    audience: &str,
) -> Result<RmcpAuthorizationStart, McpOAuthError> {
    let capture = CapturingStateStore::default();
    let mut manager =
        AuthorizationManager::new_with_oauth_http_client(audience, Arc::new(NoNetworkOAuthClient))
            .await
            .map_err(protocol_error)?;
    manager.set_metadata(metadata_for_client(client));
    manager.set_state_store(capture.clone());

    let mut request = AuthorizationRequest::new(redirect_uri)
        .with_scopes(scopes.iter().cloned())
        .with_preregistered_client(client.remote_client_id.clone())
        .with_application_type("web");
    if let Some(secret) = client_secret {
        request = request.with_client_secret(secret.expose());
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, error)| protocol_error(error))?;
    let (state, stored) = capture.take().ok_or_else(|| McpOAuthError::Protocol {
        message: "rmcp did not produce durable authorization state".to_owned(),
    })?;
    Ok(RmcpAuthorizationStart {
        authorize_url: session.get_authorization_url().to_owned(),
        state,
        pkce_verifier: SecretValue::new(stored.pkce_verifier),
        expected_issuer: stored.expected_issuer,
        require_issuer: stored.require_issuer,
    })
}

pub(crate) async fn request_rmcp_token(
    http: Arc<dyn OAuthMetadataClient>,
    request: &OAuthTokenRequest,
    context: &McpOAuthTokenContext,
) -> Result<OAuthTokenResponse, OAuthTokenError> {
    let resource = request
        .resource
        .as_deref()
        .ok_or_else(|| OAuthTokenError::InvalidResponse {
            message: "MCP OAuth token request has no resource audience".to_owned(),
        })?;
    let mut manager = AuthorizationManager::new_with_oauth_http_client(resource, http)
        .await
        .map_err(map_rmcp_token_error)?;
    manager.set_metadata(metadata_for_token_request(request, context));
    let mut client_config = OAuthClientConfig::new(
        request.remote_client_id.clone(),
        match &request.grant {
            OAuthTokenGrant::AuthorizationCode { redirect_uri, .. } => redirect_uri.clone(),
            OAuthTokenGrant::RefreshToken { .. } => resource.to_owned(),
        },
    )
    .with_scopes(context.requested_scopes.clone())
    .with_application_type("web");
    if let Some(secret) = &request.client_secret {
        client_config = client_config.with_client_secret(secret.expose());
    }
    manager
        .configure_client(client_config)
        .map_err(map_rmcp_token_error)?;

    let response = match &request.grant {
        OAuthTokenGrant::AuthorizationCode {
            code,
            code_verifier,
            ..
        } => {
            let state = context.callback_state.as_ref().ok_or_else(|| {
                OAuthTokenError::InvalidResponse {
                    message: "MCP OAuth callback state is unavailable".to_owned(),
                }
            })?;
            let state_store = CapturingStateStore::with_state(
                state.expose(),
                StoredAuthorizationState::new_with_expected_issuer(
                    &PkceCodeVerifier::new(code_verifier.expose().to_owned()),
                    &CsrfToken::new(state.expose().to_owned()),
                    context.issuer.clone(),
                    context.require_issuer,
                )
                .with_requested_scopes(context.requested_scopes.clone()),
            );
            manager.set_state_store(state_store);
            manager
                .exchange_code_for_token_with_issuer(
                    code.expose(),
                    state.expose(),
                    context.callback_issuer.as_deref(),
                )
                .await
                .map_err(map_rmcp_token_error)?
        }
        OAuthTokenGrant::RefreshToken { refresh_token } => {
            let token_response = serde_json::from_value(serde_json::json!({
                "access_token": "lightspeed-refresh-placeholder",
                "token_type": "bearer",
                "refresh_token": refresh_token.expose(),
                "scope": context.requested_scopes.join(" "),
            }))
            .map_err(|_| OAuthTokenError::InvalidResponse {
                message: "construct MCP refresh state".to_owned(),
            })?;
            let credentials = InMemoryCredentialStore::new();
            credentials
                .save(
                    StoredCredentials::new(
                        request.remote_client_id.clone(),
                        Some(token_response),
                        context.requested_scopes.clone(),
                        None,
                    )
                    .with_issuer(context.issuer.clone()),
                )
                .await
                .map_err(map_rmcp_token_error)?;
            manager.set_credential_store(credentials);
            manager
                .refresh_token()
                .await
                .map_err(map_rmcp_token_error)?
        }
    };

    let json = serde_json::to_string(&response).map_err(|_| OAuthTokenError::InvalidResponse {
        message: "decode MCP token response".to_owned(),
    })?;
    crate::oauth::parse_token_response_body(&json)
}

fn metadata_for_client(client: &OAuthClientRecord) -> AuthorizationMetadata {
    let mut additional_fields = HashMap::new();
    additional_fields.insert(
        "authorization_response_iss_parameter_supported".to_owned(),
        serde_json::Value::Bool(client.authorization_response_iss_parameter_supported),
    );
    additional_fields.insert(
        "token_endpoint_auth_methods_supported".to_owned(),
        serde_json::json!([match client.token_endpoint_auth_method {
            TokenEndpointAuthMethod::ClientSecretBasic => "client_secret_basic",
            TokenEndpointAuthMethod::ClientSecretPost => "client_secret_post",
            TokenEndpointAuthMethod::None => "none",
        }]),
    );
    let mut metadata = AuthorizationMetadata::default();
    metadata.authorization_endpoint = client.authorization_endpoint.clone();
    metadata.token_endpoint = client.token_endpoint.clone();
    metadata.issuer = client.authorization_server_issuer.clone();
    metadata.scopes_supported = Some(client.authorization_server_scopes_supported.clone());
    metadata.code_challenge_methods_supported = Some(vec!["S256".to_owned()]);
    metadata.response_types_supported = Some(vec!["code".to_owned()]);
    metadata.additional_fields = additional_fields;
    metadata
}

fn metadata_for_token_request(
    request: &OAuthTokenRequest,
    context: &McpOAuthTokenContext,
) -> AuthorizationMetadata {
    let mut additional_fields = HashMap::new();
    additional_fields.insert(
        "authorization_response_iss_parameter_supported".to_owned(),
        serde_json::Value::Bool(context.require_issuer),
    );
    additional_fields.insert(
        "token_endpoint_auth_methods_supported".to_owned(),
        serde_json::json!([match request.auth_method {
            TokenEndpointAuthMethod::ClientSecretBasic => "client_secret_basic",
            TokenEndpointAuthMethod::ClientSecretPost => "client_secret_post",
            TokenEndpointAuthMethod::None => "none",
        }]),
    );
    let mut metadata = AuthorizationMetadata::default();
    metadata.authorization_endpoint = context.authorization_endpoint.clone();
    metadata.token_endpoint = request.token_endpoint.clone();
    metadata.issuer = context.issuer.clone();
    metadata.scopes_supported = Some(context.scopes_supported.clone());
    metadata.code_challenge_methods_supported = Some(vec!["S256".to_owned()]);
    metadata.response_types_supported = Some(vec!["code".to_owned()]);
    metadata.additional_fields = additional_fields;
    metadata
}

fn map_rmcp_token_error(error: RmcpAuthError) -> OAuthTokenError {
    match error {
        RmcpAuthError::TokenRefreshRejected(_) => OAuthTokenError::InvalidGrant {
            description: Some("refresh token was rejected".to_owned()),
        },
        RmcpAuthError::AuthorizationServerMismatch { .. }
        | RmcpAuthError::AuthorizationServerMissingIssuer { .. } => OAuthTokenError::Protocol {
            error: "invalid_authorization_response_issuer".to_owned(),
            description: Some(sanitize_rmcp_error(error)),
        },
        RmcpAuthError::InsufficientScope { required_scope, .. } => OAuthTokenError::Protocol {
            error: "insufficient_scope".to_owned(),
            description: Some(format!(
                "additional consent is required for {required_scope}"
            )),
        },
        RmcpAuthError::TokenExchangeFailed(_)
        | RmcpAuthError::TokenRefreshFailed(_)
        | RmcpAuthError::HttpError(_) => OAuthTokenError::Http {
            status: None,
            message: "MCP OAuth token endpoint request failed".to_owned(),
        },
        other => OAuthTokenError::InvalidResponse {
            message: sanitize_rmcp_error(other),
        },
    }
}

#[derive(Clone, Default)]
struct CapturingStateStore {
    state: Arc<Mutex<Option<(String, StoredAuthorizationState)>>>,
}

impl CapturingStateStore {
    fn with_state(key: &str, state: StoredAuthorizationState) -> Self {
        Self {
            state: Arc::new(Mutex::new(Some((key.to_owned(), state)))),
        }
    }

    fn take(&self) -> Option<(String, StoredAuthorizationState)> {
        self.state.lock().ok()?.take()
    }
}

#[async_trait]
impl StateStore for CapturingStateStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), RmcpAuthError> {
        *self.state.lock().map_err(|_| {
            RmcpAuthError::InternalError("authorization state unavailable".to_owned())
        })? = Some((csrf_token.to_owned(), state));
        Ok(())
    }

    async fn load(
        &self,
        csrf_token: &str,
    ) -> Result<Option<StoredAuthorizationState>, RmcpAuthError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| {
                RmcpAuthError::InternalError("authorization state unavailable".to_owned())
            })?
            .as_ref()
            .filter(|(key, _)| key == csrf_token)
            .map(|(_, state)| state.clone()))
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), RmcpAuthError> {
        let mut state = self.state.lock().map_err(|_| {
            RmcpAuthError::InternalError("authorization state unavailable".to_owned())
        })?;
        if state.as_ref().is_some_and(|(key, _)| key == csrf_token) {
            *state = None;
        }
        Ok(())
    }
}

struct NoNetworkOAuthClient;

impl OAuthHttpClient for NoNetworkOAuthClient {
    fn execute(&self, _request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async { Err(Box::new(NoNetworkError) as OAuthHttpClientError) })
    }
}

#[derive(Debug, Error)]
#[error("unexpected network request while reconstructing durable OAuth state")]
struct NoNetworkError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryOAuthClientStore, InMemorySecretStore};

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        method: String,
        url: String,
        body: String,
    }

    #[derive(Default)]
    struct OAuthFixture {
        requests: Mutex<Vec<CapturedRequest>>,
        issuer_mismatch: bool,
        no_metadata: bool,
    }

    impl OAuthFixture {
        fn response(
            status: u16,
            content_type: Option<&str>,
            body: serde_json::Value,
        ) -> oauth2::HttpResponse {
            let mut response = oauth2::http::Response::builder().status(status);
            if let Some(content_type) = content_type {
                response = response.header("content-type", content_type);
            }
            response
                .body(if body.is_null() {
                    Vec::new()
                } else {
                    serde_json::to_vec(&body).unwrap()
                })
                .unwrap()
        }
    }

    impl OAuthHttpClient for OAuthFixture {
        fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
            Box::pin(async move {
                let method = request.request.method().to_string();
                let url = request.request.uri().to_string();
                let body = String::from_utf8_lossy(request.request.body()).to_string();
                self.requests.lock().unwrap().push(CapturedRequest {
                    method: method.clone(),
                    url: url.clone(),
                    body,
                });
                if self.no_metadata {
                    return Ok(Self::response(404, None, serde_json::Value::Null));
                }
                let response = match (method.as_str(), url.as_str()) {
                    ("GET", "https://mcp.example/mcp") => oauth2::http::Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Bearer resource_metadata="https://mcp.example/oauth-resource""#,
                        )
                        .body(Vec::new())
                        .unwrap(),
                    ("GET", "https://mcp.example/oauth-resource") => Self::response(
                        200,
                        Some("application/json"),
                        serde_json::json!({
                            "resource": "https://mcp.example/mcp",
                            "authorization_servers": ["https://login.example"],
                            "scopes_supported": ["tools.read"]
                        }),
                    ),
                    ("GET", "https://login.example/.well-known/oauth-authorization-server") => {
                        Self::response(
                            200,
                            Some("application/json"),
                            serde_json::json!({
                                "issuer": if self.issuer_mismatch {
                                    "https://evil.example"
                                } else {
                                    "https://login.example"
                                },
                                "authorization_endpoint": "https://login.example/authorize",
                                "token_endpoint": "https://login.example/token",
                                "registration_endpoint": "https://login.example/register",
                                "code_challenge_methods_supported": ["S256"],
                                "response_types_supported": ["code"],
                                "scopes_supported": ["tools.read", "offline_access"],
                                "token_endpoint_auth_methods_supported": ["client_secret_post"],
                                "client_id_metadata_document_supported": true,
                                "authorization_response_iss_parameter_supported": true
                            }),
                        )
                    }
                    ("POST", "https://login.example/register") => Self::response(
                        201,
                        Some("application/json"),
                        serde_json::json!({
                            "client_id": "registered-client",
                            "client_secret": "registered-secret",
                            "redirect_uris": ["https://lightspeed.example/auth/callback"]
                        }),
                    ),
                    ("POST", "https://login.example/token") => Self::response(
                        200,
                        Some("application/json"),
                        serde_json::json!({
                            "access_token": "access-token",
                            "token_type": "bearer",
                            "expires_in": 3600,
                            "refresh_token": "refresh-token",
                            "scope": "tools.read offline_access"
                        }),
                    ),
                    _ => Self::response(404, None, serde_json::Value::Null),
                };
                Ok(response)
            })
        }
    }

    fn target() -> McpOAuthTarget {
        McpOAuthTarget {
            server_id: "crm".to_owned(),
            server_url: "https://mcp.example/mcp".to_owned(),
            scopes_default: vec!["tools.read".to_owned()],
            protected_resource_metadata_url: None,
            authorization_server_hint: None,
        }
    }

    #[test]
    fn challenges_are_parsed_by_rmcp() {
        let challenge = parse_mcp_oauth_challenge(
            r#"Bearer resource_metadata="https://mcp.example/.well-known/oauth-protected-resource", error="insufficient_scope", scope="tools.read tools.write""#,
            "https://mcp.example/mcp",
        )
        .unwrap();
        assert!(challenge.insufficient_scope);
        assert!(!challenge.invalid_token);
        assert_eq!(challenge.required_scopes, ["tools.read", "tools.write"]);
        assert_eq!(
            challenge.resource_metadata_url.as_deref(),
            Some("https://mcp.example/.well-known/oauth-protected-resource")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rmcp_authorization_start_captures_state_without_persisting_it() {
        let client = OAuthClientRecord {
            client_id: OAuthClientId::new("mcp:test"),
            provider_id: "mcp:test".to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            display_name: None,
            authorization_endpoint: "https://login.example/authorize".to_owned(),
            token_endpoint: "https://login.example/token".to_owned(),
            remote_client_id: "lightspeed".to_owned(),
            client_secret: None,
            token_endpoint_auth_method: TokenEndpointAuthMethod::None,
            scopes_default: vec!["tools.read".to_owned()],
            audience: Some("https://mcp.example/mcp".to_owned()),
            authorization_server_issuer: Some("https://login.example".to_owned()),
            authorization_response_iss_parameter_supported: true,
            authorization_server_scopes_supported: vec!["tools.read".to_owned()],
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let started = begin_rmcp_authorization(
            &client,
            None,
            "https://lightspeed.example/auth/callback",
            &client.scopes_default,
            client.audience.as_deref().unwrap(),
        )
        .await
        .unwrap();
        assert!(started.authorize_url.contains("code_challenge="));
        assert!(started.authorize_url.contains("resource="));
        assert!(started.authorize_url.contains("state="));
        assert_eq!(
            started.expected_issuer.as_deref(),
            Some("https://login.example")
        );
        assert!(started.require_issuer);
        assert!(!started.state.is_empty());
        assert!(!started.pkce_verifier.expose().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rmcp_discovery_and_dcr_persist_only_durable_public_facts() {
        let clients = Arc::new(InMemoryOAuthClientStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let fixture = Arc::new(OAuthFixture::default());
        let driver = McpOAuthDriver::new(clients, secrets.clone(), fixture.clone())
            .with_now_fn(Arc::new(|| 100));

        let client = driver
            .ensure_client(&target(), "https://lightspeed.example/auth/callback", None)
            .await
            .unwrap();
        assert_eq!(client.remote_client_id, "registered-client");
        assert_eq!(
            client.token_endpoint_auth_method,
            TokenEndpointAuthMethod::ClientSecretPost
        );
        assert_eq!(
            client.authorization_server_issuer.as_deref(),
            Some("https://login.example")
        );
        assert!(client.authorization_response_iss_parameter_supported);
        assert_eq!(
            client.authorization_server_scopes_supported,
            ["tools.read", "offline_access"]
        );
        let (_, secret) = secrets
            .read_secret(client.client_secret.as_ref().unwrap())
            .await
            .unwrap();
        assert_eq!(secret.expose(), "registered-secret");

        let requests = fixture.requests.lock().unwrap();
        let registration = requests
            .iter()
            .find(|request| request.url == "https://login.example/register")
            .unwrap();
        assert_eq!(registration.method, "POST");
        assert!(registration.body.contains(r#""application_type":"web""#));
        assert!(
            registration
                .body
                .contains(r#""token_endpoint_auth_method":"none""#)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rmcp_client_selection_prefers_cimd_without_registration() {
        let clients = Arc::new(InMemoryOAuthClientStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let fixture = Arc::new(OAuthFixture::default());
        let driver = McpOAuthDriver::new(clients, secrets, fixture.clone());
        let client = driver
            .ensure_client(
                &target(),
                "https://lightspeed.example/auth/callback",
                Some(&CimdConfig {
                    client_id_url: "https://lightspeed.example/auth/client-metadata.json"
                        .to_owned(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            client.remote_client_id,
            "https://lightspeed.example/auth/client-metadata.json"
        );
        assert!(client.client_secret.is_none());
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.url != "https://login.example/register")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rmcp_rejects_legacy_endpoint_fallback_and_issuer_mismatch() {
        let driver = McpOAuthDriver::new(
            Arc::new(InMemoryOAuthClientStore::new()),
            Arc::new(InMemorySecretStore::new()),
            Arc::new(OAuthFixture {
                no_metadata: true,
                ..Default::default()
            }),
        );
        assert!(matches!(
            driver.discover_protected_resource(&target()).await,
            Err(McpOAuthError::ProtectedResourceMetadataUnavailable { .. })
        ));

        let driver = McpOAuthDriver::new(
            Arc::new(InMemoryOAuthClientStore::new()),
            Arc::new(InMemorySecretStore::new()),
            Arc::new(OAuthFixture {
                issuer_mismatch: true,
                ..Default::default()
            }),
        );
        assert!(matches!(
            driver.discover_protected_resource(&target()).await,
            Err(McpOAuthError::InvalidIssuer { .. })
        ));
    }

    /// The real client is asked for a loopback server the list does not name.
    /// The refusal has to come from the address check, before a connection is
    /// opened, or the list would not bound what OAuth discovery can probe.
    #[tokio::test(flavor = "current_thread")]
    async fn unlisted_private_server_is_refused_before_any_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let port = listener.local_addr().expect("fixture address").port();
        let http = HttpOAuthMetadataClient::with_private_networks(
            crate::PrivateNetworkList::parse("10.0.0.0/8,mcp.internal").expect("list"),
        );
        let driver = McpOAuthDriver::new(
            Arc::new(InMemoryOAuthClientStore::new()),
            Arc::new(InMemorySecretStore::new()),
            Arc::new(http),
        );
        let target = McpOAuthTarget {
            server_url: format!("http://127.0.0.1:{port}/mcp"),
            ..target()
        };

        driver
            .discover_protected_resource(&target)
            .await
            .expect_err("an unlisted private server is out of reach");
        let connection =
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept()).await;
        assert!(connection.is_err(), "no connection may be attempted");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn client_judges_each_oauth_endpoint_by_its_own_host() {
        async fn verdict(
            http: &HttpOAuthMetadataClient,
            url: &str,
        ) -> Result<(), crate::PinnedHttpError> {
            http.policy()
                .client_for_url(&reqwest::Url::parse(url).expect("url"))
                .await
                .map(|_| ())
        }

        // The MCP server is listed; an authorization server on another
        // private host is not, and being named by a listed server's metadata
        // does not make it reachable.
        let http = HttpOAuthMetadataClient::with_private_networks(
            crate::PrivateNetworkList::parse("localhost").expect("list"),
        );
        assert_eq!(verdict(&http, "http://localhost:9/mcp").await, Ok(()));
        assert_eq!(
            verdict(&http, "https://127.0.0.1:9/token").await,
            Err(crate::PinnedHttpError::NonPublicAddress)
        );
        assert_eq!(
            verdict(&http, "http://127.0.0.1:9/token").await,
            Err(crate::PinnedHttpError::HttpsRequired)
        );

        let public_only = HttpOAuthMetadataClient::new().expect("client");
        assert_eq!(
            verdict(&public_only, "http://localhost:9/mcp").await,
            Err(crate::PinnedHttpError::HttpsRequired)
        );
        assert_eq!(
            verdict(&public_only, "https://localhost:9/mcp").await,
            Err(crate::PinnedHttpError::NonPublicAddress)
        );
    }

    fn token_request() -> OAuthTokenRequest {
        OAuthTokenRequest {
            token_endpoint: "https://login.example/token".to_owned(),
            remote_client_id: "registered-client".to_owned(),
            client_secret: Some(SecretValue::new("registered-secret")),
            auth_method: TokenEndpointAuthMethod::ClientSecretPost,
            grant: OAuthTokenGrant::AuthorizationCode {
                code: SecretValue::new("authorization-code"),
                redirect_uri: "https://lightspeed.example/auth/callback".to_owned(),
                code_verifier: SecretValue::new("pkce-verifier"),
            },
            resource: Some("https://mcp.example/mcp".to_owned()),
            mcp: None,
        }
    }

    fn token_context(callback_issuer: Option<&str>) -> McpOAuthTokenContext {
        McpOAuthTokenContext {
            authorization_endpoint: "https://login.example/authorize".to_owned(),
            issuer: Some("https://login.example".to_owned()),
            require_issuer: true,
            scopes_supported: vec!["tools.read".to_owned(), "offline_access".to_owned()],
            requested_scopes: vec!["tools.read".to_owned()],
            callback_state: Some(SecretValue::new("callback-state")),
            callback_issuer: callback_issuer.map(str::to_owned),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rmcp_token_exchange_validates_issuer_and_binds_resource() {
        let fixture = Arc::new(OAuthFixture::default());
        let error = request_rmcp_token(
            fixture.clone(),
            &token_request(),
            &token_context(Some("https://evil.example")),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            OAuthTokenError::Protocol { ref error, .. }
                if error == "invalid_authorization_response_issuer"
        ));
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.url != "https://login.example/token")
        );

        let response = request_rmcp_token(
            fixture.clone(),
            &token_request(),
            &token_context(Some("https://login.example")),
        )
        .await
        .unwrap();
        assert_eq!(response.access_token.expose(), "access-token");
        let requests = fixture.requests.lock().unwrap();
        let token = requests
            .iter()
            .find(|request| request.url == "https://login.example/token")
            .unwrap();
        assert!(
            token
                .body
                .contains("resource=https%3A%2F%2Fmcp.example%2Fmcp")
        );
        assert!(token.body.contains("code_verifier=pkce-verifier"));
        assert!(token.body.contains("client_secret=registered-secret"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rmcp_requires_advertised_callback_issuer_and_refreshes_with_resource() {
        let fixture = Arc::new(OAuthFixture::default());
        let error = request_rmcp_token(fixture.clone(), &token_request(), &token_context(None))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OAuthTokenError::Protocol { ref error, .. }
                if error == "invalid_authorization_response_issuer"
        ));

        let refresh = OAuthTokenRequest {
            token_endpoint: "https://login.example/token".to_owned(),
            remote_client_id: "registered-client".to_owned(),
            client_secret: Some(SecretValue::new("registered-secret")),
            auth_method: TokenEndpointAuthMethod::ClientSecretPost,
            grant: OAuthTokenGrant::RefreshToken {
                refresh_token: SecretValue::new("old-refresh-token"),
            },
            resource: Some("https://mcp.example/mcp".to_owned()),
            mcp: None,
        };
        let mut context = token_context(None);
        context.callback_state = None;
        let response = request_rmcp_token(fixture.clone(), &refresh, &context)
            .await
            .unwrap();
        assert_eq!(response.refresh_token.unwrap().expose(), "refresh-token");
        let requests = fixture.requests.lock().unwrap();
        let token = requests
            .iter()
            .rev()
            .find(|request| request.url == "https://login.example/token")
            .unwrap();
        assert!(token.body.contains("refresh_token=old-refresh-token"));
        assert!(
            token
                .body
                .contains("resource=https%3A%2F%2Fmcp.example%2Fmcp")
        );
        assert!(token.body.contains("scope=tools.read"));
    }
}
