//! Generic OAuth substrate (P69 G2): client records, authorization flows,
//! PKCE helpers, and the token-endpoint client used for code exchange and
//! refresh.
//!
//! Protocol mechanics live here behind the [`OAuthTokenClient`] trait; durable
//! state crosses the [`OAuthClientStore`]/[`AuthFlowStore`] traits. Token and
//! verifier values only ever move inside [`SecretValue`] wrappers.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AuthFlowId, AuthGrantId, AuthProviderKind, AuthRegistryError, OAuthClientId, PrincipalRef,
    SecretId, SecretValue, validate_audience_url, validate_nonempty_optional,
    validate_nonnegative_i64, validate_oauth_endpoint_url,
    validate_scopes, validate_token_component,
};

pub const SECRET_KIND_OAUTH_ACCESS_TOKEN: &str = "auth.oauth.access_token";
pub const SECRET_KIND_OAUTH_REFRESH_TOKEN: &str = "auth.oauth.refresh_token";
pub const SECRET_KIND_OAUTH_CLIENT_SECRET: &str = "auth.oauth.client_secret";
pub const SECRET_KIND_OAUTH_PKCE_VERIFIER: &str = "auth.oauth.pkce_verifier";

/// How the client authenticates against the token endpoint (RFC 6749 §2.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenEndpointAuthMethod {
    #[default]
    #[serde(rename = "client_secret_basic")]
    ClientSecretBasic,
    #[serde(rename = "client_secret_post")]
    ClientSecretPost,
    #[serde(rename = "none")]
    None,
}

pub(crate) fn is_oauth_provider_kind(kind: AuthProviderKind) -> bool {
    matches!(
        kind,
        AuthProviderKind::McpOAuth
            | AuthProviderKind::GitHubAppUser
            | AuthProviderKind::GitHubOAuthApp
            | AuthProviderKind::CustomOAuth
    )
}

/// A manually configured OAuth client: authorization/token endpoint metadata
/// plus the AS-issued client identifier. Discovery, DCR, and CIMD arrive with
/// the MCP OAuth driver (P69 G4). The client secret, when present, lives in
/// the secret store and is referenced here by id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthClientRecord {
    /// Lightspeed catalog id for this client configuration.
    pub client_id: OAuthClientId,
    /// Logical provider id recorded on grants minted through this client.
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub display_name: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// The client identifier issued by the authorization server.
    pub remote_client_id: String,
    pub client_secret: Option<SecretId>,
    pub token_endpoint_auth_method: TokenEndpointAuthMethod,
    pub scopes_default: Vec<String>,
    /// Default resource grants are bound to (RFC 8707 resource).
    pub audience: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl OAuthClientRecord {
    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        validate_token_component("provider id", &self.provider_id)?;
        if !is_oauth_provider_kind(self.provider_kind) {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "provider kind {:?} is not an OAuth kind",
                    self.provider_kind
                ),
            });
        }
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_oauth_endpoint_url("authorization endpoint", &self.authorization_endpoint)?;
        validate_oauth_endpoint_url("token endpoint", &self.token_endpoint)?;
        validate_token_component("remote client id", &self.remote_client_id)?;
        validate_scopes(&self.scopes_default)?;
        if let Some(audience) = &self.audience {
            validate_audience_url(audience)?;
        }
        if self.provider_kind == AuthProviderKind::McpOAuth && self.audience.is_none() {
            return Err(AuthRegistryError::InvalidInput {
                message: "mcp_oauth clients require an audience (the MCP server resource URL)"
                    .to_owned(),
            });
        }
        match (self.token_endpoint_auth_method, &self.client_secret) {
            (TokenEndpointAuthMethod::None, Some(_)) => Err(AuthRegistryError::InvalidInput {
                message: "token endpoint auth method 'none' must not carry a client secret"
                    .to_owned(),
            }),
            (
                TokenEndpointAuthMethod::ClientSecretBasic | TokenEndpointAuthMethod::ClientSecretPost,
                None,
            ) => Err(AuthRegistryError::InvalidInput {
                message: "client_secret_basic/client_secret_post require a client secret"
                    .to_owned(),
            }),
            _ => {
                validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
                validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOAuthClientRecord {
    pub client_id: OAuthClientId,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub display_name: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub remote_client_id: String,
    pub client_secret: Option<SecretId>,
    pub token_endpoint_auth_method: TokenEndpointAuthMethod,
    pub scopes_default: Vec<String>,
    pub audience: Option<String>,
    pub created_at_ms: i64,
}

impl CreateOAuthClientRecord {
    pub fn into_record(self) -> OAuthClientRecord {
        OAuthClientRecord {
            client_id: self.client_id,
            provider_id: self.provider_id,
            provider_kind: self.provider_kind,
            display_name: self.display_name,
            authorization_endpoint: self.authorization_endpoint,
            token_endpoint: self.token_endpoint,
            remote_client_id: self.remote_client_id,
            client_secret: self.client_secret,
            token_endpoint_auth_method: self.token_endpoint_auth_method,
            scopes_default: self.scopes_default,
            audience: self.audience,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

#[async_trait]
pub trait OAuthClientStore: Send + Sync {
    async fn create_oauth_client(
        &self,
        record: CreateOAuthClientRecord,
    ) -> Result<OAuthClientRecord, AuthRegistryError>;

    async fn read_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, AuthRegistryError>;

    async fn list_oauth_clients(&self) -> Result<Vec<OAuthClientRecord>, AuthRegistryError>;

    async fn delete_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, AuthRegistryError>;
}

/// Derived lifecycle state of an authorization flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFlowStatus {
    Pending,
    Completed,
    Failed,
    Expired,
}

/// A one-time-use authorization-code flow. The `state` parameter is never
/// stored; only its SHA-256 hash is, and the PKCE verifier lives in the
/// secret store. Completing a flow consumes it: a second callback with the
/// same state fails, and expired flows cannot be completed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthFlowRecord {
    pub flow_id: AuthFlowId,
    pub client_id: OAuthClientId,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRef,
    /// Lowercase hex SHA-256 of the `state` query parameter.
    pub state_hash: String,
    pub pkce_verifier_secret: SecretId,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
    /// Set when the flow completed successfully.
    pub grant_id: Option<AuthGrantId>,
    /// Set when the flow completed with a failure.
    pub error: Option<String>,
    pub expires_at_ms: i64,
    pub consumed_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl AuthFlowRecord {
    pub fn status(&self, now_ms: i64) -> AuthFlowStatus {
        if self.completed_at_ms.is_some() {
            if self.grant_id.is_some() {
                AuthFlowStatus::Completed
            } else {
                AuthFlowStatus::Failed
            }
        } else if now_ms >= self.expires_at_ms {
            AuthFlowStatus::Expired
        } else {
            AuthFlowStatus::Pending
        }
    }

    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        validate_token_component("provider id", &self.provider_id)?;
        if !is_oauth_provider_kind(self.provider_kind) {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "provider kind {:?} is not an OAuth kind",
                    self.provider_kind
                ),
            });
        }
        self.principal.validate()?;
        validate_token_component("state hash", &self.state_hash)?;
        validate_audience_url(&self.redirect_uri).map_err(|error| match error {
            AuthRegistryError::InvalidInput { message } => AuthRegistryError::InvalidInput {
                message: format!("redirect uri: {message}"),
            },
            other => other,
        })?;
        validate_scopes(&self.scopes)?;
        if let Some(audience) = &self.audience {
            validate_audience_url(audience)?;
        }
        if self.grant_id.is_some() && self.error.is_some() {
            return Err(AuthRegistryError::InvalidInput {
                message: "auth flow cannot carry both a grant id and an error".to_owned(),
            });
        }
        if (self.grant_id.is_some() || self.error.is_some()) && self.completed_at_ms.is_none() {
            return Err(AuthRegistryError::InvalidInput {
                message: "auth flow outcome requires completed_at_ms".to_owned(),
            });
        }
        validate_nonempty_optional("error", self.error.as_deref())?;
        validate_nonnegative_i64(self.expires_at_ms, "expires_at_ms")?;
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAuthFlowRecord {
    pub flow_id: AuthFlowId,
    pub client_id: OAuthClientId,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRef,
    pub state_hash: String,
    pub pkce_verifier_secret: SecretId,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
}

impl CreateAuthFlowRecord {
    pub fn into_record(self) -> AuthFlowRecord {
        AuthFlowRecord {
            flow_id: self.flow_id,
            client_id: self.client_id,
            provider_id: self.provider_id,
            provider_kind: self.provider_kind,
            principal: self.principal,
            state_hash: self.state_hash,
            pkce_verifier_secret: self.pkce_verifier_secret,
            redirect_uri: self.redirect_uri,
            scopes: self.scopes,
            audience: self.audience,
            grant_id: None,
            error: None,
            expires_at_ms: self.expires_at_ms,
            consumed_at_ms: None,
            completed_at_ms: None,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

/// Terminal outcome for a consumed flow: exactly one of `grant_id` / `error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishAuthFlow {
    pub grant_id: Option<AuthGrantId>,
    pub error: Option<String>,
    pub completed_at_ms: i64,
}

impl FinishAuthFlow {
    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        if self.grant_id.is_some() == self.error.is_some() {
            return Err(AuthRegistryError::InvalidInput {
                message: "auth flow outcome must set exactly one of grant id or error".to_owned(),
            });
        }
        validate_nonempty_optional("error", self.error.as_deref())?;
        validate_nonnegative_i64(self.completed_at_ms, "completed_at_ms")?;
        Ok(())
    }
}

#[async_trait]
pub trait AuthFlowStore: Send + Sync {
    async fn create_flow(
        &self,
        record: CreateAuthFlowRecord,
    ) -> Result<AuthFlowRecord, AuthRegistryError>;

    async fn read_flow(&self, flow_id: &AuthFlowId) -> Result<AuthFlowRecord, AuthRegistryError>;

    async fn read_flow_by_state_hash(
        &self,
        state_hash: &str,
    ) -> Result<Option<AuthFlowRecord>, AuthRegistryError>;

    /// Atomically mark the flow consumed. Fails with
    /// [`AuthRegistryError::FlowAlreadyConsumed`] when a callback already
    /// claimed it and [`AuthRegistryError::FlowExpired`] when past its TTL,
    /// so duplicate or late callbacks cannot race a code exchange.
    async fn consume_flow(
        &self,
        flow_id: &AuthFlowId,
        now_ms: i64,
    ) -> Result<AuthFlowRecord, AuthRegistryError>;

    /// Record the terminal outcome of a consumed flow. Fails with
    /// [`AuthRegistryError::FlowAlreadyCompleted`] when an outcome exists.
    async fn finish_flow(
        &self,
        flow_id: &AuthFlowId,
        outcome: FinishAuthFlow,
    ) -> Result<AuthFlowRecord, AuthRegistryError>;
}

/// Generate the OAuth `state` parameter: 256 bits from the OS RNG,
/// base64url-encoded. The raw value is returned to the caller (inside the
/// authorization URL) and only its hash is persisted.
pub fn generate_state() -> String {
    random_url_safe(32)
}

/// Lowercase hex SHA-256 of a `state` value, used as the stored lookup key.
pub fn state_hash(state: &str) -> String {
    hex::encode(Sha256::digest(state.as_bytes()))
}

/// Generate a PKCE code verifier (RFC 7636 §4.1): 256 bits from the OS RNG,
/// base64url-encoded to 43 characters.
pub fn generate_pkce_verifier() -> SecretValue {
    SecretValue::new(random_url_safe(32))
}

/// S256 code challenge for a PKCE verifier (RFC 7636 §4.2).
pub fn pkce_challenge_s256(verifier: &SecretValue) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose().as_bytes()))
}

fn random_url_safe(len: usize) -> String {
    use rand::RngCore;

    let mut bytes = vec![0u8; len];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the authorization-code request URL for a client and flow inputs.
/// Always sends PKCE S256 and `state`; sends `scope` when scopes are present
/// and `resource` (RFC 8707) when an audience is bound.
pub fn build_authorization_url(
    client: &OAuthClientRecord,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    code_challenge: &str,
    audience: Option<&str>,
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("response_type", "code".to_owned()),
        ("client_id", client.remote_client_id.clone()),
        ("redirect_uri", redirect_uri.to_owned()),
        ("state", state.to_owned()),
        ("code_challenge", code_challenge.to_owned()),
        ("code_challenge_method", "S256".to_owned()),
    ];
    if !scopes.is_empty() {
        params.push(("scope", scopes.join(" ")));
    }
    if let Some(audience) = audience {
        params.push(("resource", audience.to_owned()));
    }
    let separator = if client.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let query = params
        .iter()
        .map(|(key, value)| format!("{key}={}", url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}{separator}{query}", client.authorization_endpoint)
}

/// Percent-encode a URL query component (RFC 3986 unreserved set).
pub(crate) fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            other => {
                encoded.push('%');
                encoded.push_str(&format!("{other:02X}"));
            }
        }
    }
    encoded
}

/// Token endpoint request: authorization-code exchange or refresh.
/// All secret-bearing fields are [`SecretValue`]s, so derived `Debug` output
/// stays redacted.
#[derive(Clone, Debug)]
pub struct OAuthTokenRequest {
    pub token_endpoint: String,
    pub remote_client_id: String,
    pub client_secret: Option<SecretValue>,
    pub auth_method: TokenEndpointAuthMethod,
    pub grant: OAuthTokenGrant,
    /// RFC 8707 resource indicator, sent when the grant is audience-bound.
    pub resource: Option<String>,
}

#[derive(Clone, Debug)]
pub enum OAuthTokenGrant {
    AuthorizationCode {
        code: SecretValue,
        redirect_uri: String,
        code_verifier: SecretValue,
    },
    RefreshToken { refresh_token: SecretValue },
}

#[derive(Clone, Debug)]
pub struct OAuthTokenResponse {
    pub access_token: SecretValue,
    pub token_type: String,
    pub expires_in_secs: Option<i64>,
    pub refresh_token: Option<SecretValue>,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthTokenError {
    /// The authorization server rejected the grant itself (`invalid_grant`):
    /// the code or refresh token is invalid, expired, or revoked. Callers
    /// should mark the grant as needing reauthorization.
    #[error("authorization server rejected the grant (invalid_grant){}", format_description(.description))]
    InvalidGrant { description: Option<String> },

    /// Any other structured OAuth error response.
    #[error("authorization server returned {error}{}", format_description(.description))]
    Protocol {
        error: String,
        description: Option<String>,
    },

    /// Transport-level or non-OAuth HTTP failure.
    #[error("token endpoint request failed{}: {message}", .status.map(|status| format!(" with status {status}")).unwrap_or_default())]
    Http {
        status: Option<u16>,
        message: String,
    },

    /// The endpoint answered 2xx but the body is not a usable token response.
    #[error("token endpoint returned an invalid response: {message}")]
    InvalidResponse { message: String },
}

fn format_description(description: &Option<String>) -> String {
    description
        .as_ref()
        .map(|description| format!(": {description}"))
        .unwrap_or_default()
}

/// Protocol client for the OAuth token endpoint. Mocked in tests; the real
/// implementation is [`HttpOAuthTokenClient`].
#[async_trait]
pub trait OAuthTokenClient: Send + Sync {
    async fn request_token(
        &self,
        request: &OAuthTokenRequest,
    ) -> Result<OAuthTokenResponse, OAuthTokenError>;
}

/// Materialized wire form of a token request: form fields plus optional HTTP
/// basic credentials. Split out so encoding rules are unit-testable without a
/// server.
pub(crate) struct TokenRequestWire {
    pub(crate) form: Vec<(&'static str, String)>,
    pub(crate) basic_auth: Option<(String, String)>,
}

pub(crate) fn token_request_wire(
    request: &OAuthTokenRequest,
) -> Result<TokenRequestWire, OAuthTokenError> {
    let mut form: Vec<(&'static str, String)> = Vec::new();
    match &request.grant {
        OAuthTokenGrant::AuthorizationCode {
            code,
            redirect_uri,
            code_verifier,
        } => {
            form.push(("grant_type", "authorization_code".to_owned()));
            form.push(("code", code.expose().to_owned()));
            form.push(("redirect_uri", redirect_uri.clone()));
            form.push(("code_verifier", code_verifier.expose().to_owned()));
        }
        OAuthTokenGrant::RefreshToken { refresh_token } => {
            form.push(("grant_type", "refresh_token".to_owned()));
            form.push(("refresh_token", refresh_token.expose().to_owned()));
        }
    }
    if let Some(resource) = &request.resource {
        form.push(("resource", resource.clone()));
    }
    let basic_auth = match request.auth_method {
        TokenEndpointAuthMethod::ClientSecretBasic => {
            let Some(secret) = &request.client_secret else {
                return Err(OAuthTokenError::InvalidResponse {
                    message: "client_secret_basic requires a client secret".to_owned(),
                });
            };
            Some((
                request.remote_client_id.clone(),
                secret.expose().to_owned(),
            ))
        }
        TokenEndpointAuthMethod::ClientSecretPost => {
            let Some(secret) = &request.client_secret else {
                return Err(OAuthTokenError::InvalidResponse {
                    message: "client_secret_post requires a client secret".to_owned(),
                });
            };
            form.push(("client_id", request.remote_client_id.clone()));
            form.push(("client_secret", secret.expose().to_owned()));
            None
        }
        TokenEndpointAuthMethod::None => {
            form.push(("client_id", request.remote_client_id.clone()));
            None
        }
    };
    Ok(TokenRequestWire { form, basic_auth })
}

pub(crate) fn parse_token_response_body(body: &str) -> Result<OAuthTokenResponse, OAuthTokenError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| OAuthTokenError::InvalidResponse {
            message: "token response is not valid JSON".to_owned(),
        })?;
    let Some(access_token) = value.get("access_token").and_then(|token| token.as_str()) else {
        return Err(OAuthTokenError::InvalidResponse {
            message: "token response is missing access_token".to_owned(),
        });
    };
    if access_token.is_empty() {
        return Err(OAuthTokenError::InvalidResponse {
            message: "token response access_token is empty".to_owned(),
        });
    }
    let token_type = value
        .get("token_type")
        .and_then(|token_type| token_type.as_str())
        .unwrap_or("bearer")
        .to_owned();
    let expires_in_secs = value.get("expires_in").and_then(json_i64);
    if let Some(expires_in_secs) = expires_in_secs {
        if expires_in_secs < 0 {
            return Err(OAuthTokenError::InvalidResponse {
                message: "token response expires_in is negative".to_owned(),
            });
        }
    }
    let refresh_token = value
        .get("refresh_token")
        .and_then(|token| token.as_str())
        .filter(|token| !token.is_empty())
        .map(SecretValue::new);
    let scope = value
        .get("scope")
        .and_then(|scope| scope.as_str())
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned);
    Ok(OAuthTokenResponse {
        access_token: SecretValue::new(access_token),
        token_type,
        expires_in_secs,
        refresh_token,
        scope,
    })
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

pub(crate) fn parse_token_error_body(status: u16, body: &str) -> OAuthTokenError {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
            let description = value
                .get("error_description")
                .and_then(|description| description.as_str())
                .map(str::to_owned);
            if error == "invalid_grant" {
                return OAuthTokenError::InvalidGrant { description };
            }
            return OAuthTokenError::Protocol {
                error: error.to_owned(),
                description,
            };
        }
    }
    // The body is unparsed and could contain anything; never echo it.
    OAuthTokenError::Http {
        status: Some(status),
        message: "token endpoint returned a non-OAuth error body".to_owned(),
    }
}

/// Real token-endpoint client over HTTPS. Sends
/// `application/x-www-form-urlencoded` requests with `Accept:
/// application/json`, follows no redirects, and never logs request bodies.
pub struct HttpOAuthTokenClient {
    http: reqwest::Client,
}

impl HttpOAuthTokenClient {
    pub fn new() -> Result<Self, OAuthTokenError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| OAuthTokenError::Http {
                status: None,
                message: format!("build http client: {error}"),
            })?;
        Ok(Self { http })
    }
}

#[async_trait]
impl OAuthTokenClient for HttpOAuthTokenClient {
    async fn request_token(
        &self,
        request: &OAuthTokenRequest,
    ) -> Result<OAuthTokenResponse, OAuthTokenError> {
        let wire = token_request_wire(request)?;
        let mut builder = self
            .http
            .post(&request.token_endpoint)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&wire.form);
        if let Some((username, password)) = &wire.basic_auth {
            builder = builder.basic_auth(username, Some(password));
        }
        let response = builder
            .send()
            .await
            .map_err(|error| OAuthTokenError::Http {
                status: error.status().map(|status| status.as_u16()),
                message: format!("token endpoint request failed: {error}"),
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| OAuthTokenError::Http {
            status: Some(status.as_u16()),
            message: format!("read token endpoint response: {error}"),
        })?;
        if !status.is_success() {
            return Err(parse_token_error_body(status.as_u16(), &body));
        }
        parse_token_response_body(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_record() -> OAuthClientRecord {
        OAuthClientRecord {
            client_id: OAuthClientId::new("github"),
            provider_id: "github".to_owned(),
            provider_kind: AuthProviderKind::GitHubOAuthApp,
            display_name: None,
            authorization_endpoint: "https://github.com/login/oauth/authorize".to_owned(),
            token_endpoint: "https://github.com/login/oauth/access_token".to_owned(),
            remote_client_id: "Iv1.abc123".to_owned(),
            client_secret: Some(SecretId::new("authsec_client")),
            token_endpoint_auth_method: TokenEndpointAuthMethod::ClientSecretBasic,
            scopes_default: vec!["repo".to_owned()],
            audience: None,
            created_at_ms: 10,
            updated_at_ms: 10,
        }
    }

    #[test]
    fn oauth_client_records_validate() {
        client_record().validate().expect("valid client record");
    }

    #[test]
    fn oauth_client_records_reject_non_oauth_kinds() {
        let mut record = client_record();
        record.provider_kind = AuthProviderKind::StaticBearer;

        assert!(matches!(
            record.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn oauth_client_records_reject_http_endpoints_for_remote_hosts() {
        let mut record = client_record();
        record.token_endpoint = "http://github.com/login/oauth/access_token".to_owned();

        assert!(matches!(
            record.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn oauth_client_records_allow_http_loopback_endpoints() {
        let mut record = client_record();
        record.authorization_endpoint = "http://127.0.0.1:9000/authorize".to_owned();
        record.token_endpoint = "http://localhost:9000/token".to_owned();

        record.validate().expect("loopback http endpoints allowed");
    }

    #[test]
    fn oauth_client_records_require_secret_matching_auth_method() {
        let mut missing_secret = client_record();
        missing_secret.client_secret = None;
        assert!(matches!(
            missing_secret.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));

        let mut public_with_secret = client_record();
        public_with_secret.token_endpoint_auth_method = TokenEndpointAuthMethod::None;
        assert!(matches!(
            public_with_secret.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));

        let mut public_client = client_record();
        public_client.client_secret = None;
        public_client.token_endpoint_auth_method = TokenEndpointAuthMethod::None;
        public_client.validate().expect("public client allowed");
    }

    #[test]
    fn mcp_oauth_clients_require_an_audience() {
        let mut record = client_record();
        record.provider_kind = AuthProviderKind::McpOAuth;
        record.audience = None;

        assert!(matches!(
            record.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));

        record.audience = Some("https://crm.example.com/mcp".to_owned());
        record.validate().expect("audience-bound mcp oauth client");
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_appendix_b_vector() {
        let verifier = SecretValue::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");

        let challenge = pkce_challenge_s256(&verifier);

        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_state_and_verifier_are_distinct_and_url_safe() {
        let state = generate_state();
        let verifier = generate_pkce_verifier();

        assert_eq!(state.len(), 43);
        assert_eq!(verifier.expose().len(), 43);
        assert_ne!(state, verifier.expose());
        assert!(
            state
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        );
    }

    #[test]
    fn authorization_urls_carry_pkce_state_scope_and_resource() {
        let client = client_record();

        let url = build_authorization_url(
            &client,
            "https://lightspeed.example.com/auth/callback",
            &["repo".to_owned(), "read:user".to_owned()],
            "state-123",
            "challenge-456",
            Some("https://crm.example.com/mcp"),
        );

        assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=Iv1.abc123"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Flightspeed.example.com%2Fauth%2Fcallback"));
        assert!(url.contains("state=state-123"));
        assert!(url.contains("code_challenge=challenge-456"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=repo%20read%3Auser"));
        assert!(url.contains("resource=https%3A%2F%2Fcrm.example.com%2Fmcp"));
    }

    #[test]
    fn flow_status_derives_from_outcome_and_expiry() {
        let mut flow = CreateAuthFlowRecord {
            flow_id: AuthFlowId::new("authflow_1"),
            client_id: OAuthClientId::new("github"),
            provider_id: "github".to_owned(),
            provider_kind: AuthProviderKind::GitHubOAuthApp,
            principal: PrincipalRef::universe_default(),
            state_hash: state_hash("state-123"),
            pkce_verifier_secret: SecretId::new("authsec_pkce"),
            redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
            scopes: Vec::new(),
            audience: None,
            expires_at_ms: 100,
            created_at_ms: 10,
        }
        .into_record();

        assert_eq!(flow.status(50), AuthFlowStatus::Pending);
        assert_eq!(flow.status(100), AuthFlowStatus::Expired);

        flow.completed_at_ms = Some(60);
        flow.grant_id = Some(AuthGrantId::new("authgrant_1"));
        assert_eq!(flow.status(200), AuthFlowStatus::Completed);

        flow.grant_id = None;
        flow.error = Some("access_denied".to_owned());
        assert_eq!(flow.status(70), AuthFlowStatus::Failed);
    }

    #[test]
    fn token_request_wire_encodes_basic_auth_without_body_credentials() {
        let request = OAuthTokenRequest {
            token_endpoint: "https://as.example.com/token".to_owned(),
            remote_client_id: "client-1".to_owned(),
            client_secret: Some(SecretValue::new("secret-1")),
            auth_method: TokenEndpointAuthMethod::ClientSecretBasic,
            grant: OAuthTokenGrant::AuthorizationCode {
                code: SecretValue::new("code-1"),
                redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
                code_verifier: SecretValue::new("verifier-1"),
            },
            resource: Some("https://crm.example.com/mcp".to_owned()),
        };

        let wire = token_request_wire(&request).expect("wire form");

        assert_eq!(wire.basic_auth, Some(("client-1".to_owned(), "secret-1".to_owned())));
        assert!(wire.form.contains(&("grant_type", "authorization_code".to_owned())));
        assert!(wire.form.contains(&("code", "code-1".to_owned())));
        assert!(wire.form.contains(&("code_verifier", "verifier-1".to_owned())));
        assert!(wire.form.contains(&("resource", "https://crm.example.com/mcp".to_owned())));
        assert!(!wire.form.iter().any(|(key, _)| *key == "client_secret"));
        assert!(!wire.form.iter().any(|(key, _)| *key == "client_id"));
    }

    #[test]
    fn token_request_wire_posts_credentials_for_client_secret_post() {
        let request = OAuthTokenRequest {
            token_endpoint: "https://as.example.com/token".to_owned(),
            remote_client_id: "client-1".to_owned(),
            client_secret: Some(SecretValue::new("secret-1")),
            auth_method: TokenEndpointAuthMethod::ClientSecretPost,
            grant: OAuthTokenGrant::RefreshToken {
                refresh_token: SecretValue::new("refresh-1"),
            },
            resource: None,
        };

        let wire = token_request_wire(&request).expect("wire form");

        assert_eq!(wire.basic_auth, None);
        assert!(wire.form.contains(&("grant_type", "refresh_token".to_owned())));
        assert!(wire.form.contains(&("refresh_token", "refresh-1".to_owned())));
        assert!(wire.form.contains(&("client_id", "client-1".to_owned())));
        assert!(wire.form.contains(&("client_secret", "secret-1".to_owned())));
    }

    #[test]
    fn token_responses_parse_with_numeric_and_string_expiry() {
        let parsed = parse_token_response_body(
            r#"{"access_token":"at-1","token_type":"Bearer","expires_in":3600,"refresh_token":"rt-1","scope":"repo"}"#,
        )
        .expect("parse token response");
        assert_eq!(parsed.access_token.expose(), "at-1");
        assert_eq!(parsed.expires_in_secs, Some(3600));
        assert_eq!(parsed.refresh_token.as_ref().map(SecretValue::expose), Some("rt-1"));
        assert_eq!(parsed.scope.as_deref(), Some("repo"));

        let parsed = parse_token_response_body(r#"{"access_token":"at-2","expires_in":"7200"}"#)
            .expect("parse token response with string expiry");
        assert_eq!(parsed.token_type, "bearer");
        assert_eq!(parsed.expires_in_secs, Some(7200));
        assert!(parsed.refresh_token.is_none());

        assert!(matches!(
            parse_token_response_body(r#"{"token_type":"bearer"}"#),
            Err(OAuthTokenError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn token_error_bodies_classify_invalid_grant() {
        assert!(matches!(
            parse_token_error_body(400, r#"{"error":"invalid_grant","error_description":"revoked"}"#),
            OAuthTokenError::InvalidGrant { description: Some(description) } if description == "revoked"
        ));
        assert!(matches!(
            parse_token_error_body(400, r#"{"error":"invalid_scope"}"#),
            OAuthTokenError::Protocol { error, .. } if error == "invalid_scope"
        ));
        assert!(matches!(
            parse_token_error_body(502, "<html>bad gateway</html>"),
            OAuthTokenError::Http { status: Some(502), .. }
        ));
    }

    #[test]
    fn token_requests_redact_secrets_in_debug_output() {
        let request = OAuthTokenRequest {
            token_endpoint: "https://as.example.com/token".to_owned(),
            remote_client_id: "client-1".to_owned(),
            client_secret: Some(SecretValue::new("secret-1")),
            auth_method: TokenEndpointAuthMethod::ClientSecretBasic,
            grant: OAuthTokenGrant::RefreshToken {
                refresh_token: SecretValue::new("refresh-1"),
            },
            resource: None,
        };

        let debug = format!("{request:?}");

        assert!(!debug.contains("secret-1"));
        assert!(!debug.contains("refresh-1"));
        assert!(debug.contains("<redacted>"));
    }
}
