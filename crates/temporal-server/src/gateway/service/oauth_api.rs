//! OAuth client and authorization flow API helpers (P69 G2).
//!
//! Maps between `api` DTOs and `auth-registry` records. The client secret in
//! `auth/clients/create` params is the second deliberate inbound-plaintext
//! path: it is drafted into an encrypted secret record here and never appears
//! in views or logs.

use super::*;

pub(super) fn parse_oauth_client_id(
    client_id: String,
) -> Result<auth_registry::OAuthClientId, AgentApiError> {
    auth_registry::OAuthClientId::try_new(client_id).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid oauth client id: {error}"))
    })
}

pub(super) fn parse_auth_flow_id(
    flow_id: String,
) -> Result<auth_registry::AuthFlowId, AgentApiError> {
    auth_registry::AuthFlowId::try_new(flow_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid auth flow id: {error}")))
}

#[derive(Debug)]
pub(super) struct AuthClientCreateDraft {
    pub(super) secret: Option<auth_registry::PutSecretRecord>,
    pub(super) client: auth_registry::CreateOAuthClientRecord,
}

pub(super) fn auth_client_create_draft(
    params: AuthClientCreateParams,
    now_ms: i64,
) -> Result<AuthClientCreateDraft, AgentApiError> {
    let client_id = match params.client_id {
        Some(client_id) => parse_oauth_client_id(client_id)?,
        None => auth_registry::OAuthClientId::try_new(auth_registry::random_auth_id("authclient_"))
            .map_err(|error| {
                AgentApiError::internal(format!("generate oauth client id: {error}"))
            })?,
    };
    let auth_method = params.token_endpoint_auth_method.map_or_else(
        || {
            if params.client_secret.is_some() {
                auth_registry::TokenEndpointAuthMethod::ClientSecretBasic
            } else {
                auth_registry::TokenEndpointAuthMethod::None
            }
        },
        registry_token_endpoint_auth_method,
    );
    let secret = params
        .client_secret
        .map(|client_secret| {
            let secret_id =
                auth_registry::SecretId::try_new(auth_registry::random_auth_id("authsec_"))
                    .map_err(|error| {
                        AgentApiError::internal(format!("generate secret id: {error}"))
                    })?;
            Ok::<_, AgentApiError>(auth_registry::PutSecretRecord {
                secret_id,
                secret_kind: auth_registry::SECRET_KIND_OAUTH_CLIENT_SECRET.to_owned(),
                value: auth_registry::SecretValue::new(client_secret),
                created_at_ms: now_ms,
            })
        })
        .transpose()?;

    let client = auth_registry::CreateOAuthClientRecord {
        client_id: client_id.clone(),
        provider_id: params
            .provider_id
            .unwrap_or_else(|| client_id.as_str().to_owned()),
        provider_kind: registry_auth_provider_kind(params.provider_kind),
        display_name: params.display_name,
        authorization_endpoint: params.authorization_endpoint,
        token_endpoint: params.token_endpoint,
        remote_client_id: params.remote_client_id,
        client_secret: secret
            .as_ref()
            .map(|secret| secret.secret_id.clone()),
        token_endpoint_auth_method: auth_method,
        scopes_default: params.scopes_default,
        audience: params.audience,
        created_at_ms: now_ms,
    };
    if let Some(secret) = &secret {
        secret.validate().map_err(map_auth_registry_error)?;
    }
    client
        .clone()
        .into_record()
        .validate()
        .map_err(map_auth_registry_error)?;
    Ok(AuthClientCreateDraft { secret, client })
}

pub(super) fn oauth_client_view(record: auth_registry::OAuthClientRecord) -> api::OAuthClientView {
    api::OAuthClientView {
        client_id: record.client_id.as_str().to_owned(),
        provider_id: record.provider_id,
        provider_kind: api_auth_provider_kind(record.provider_kind),
        display_name: record.display_name,
        authorization_endpoint: record.authorization_endpoint,
        token_endpoint: record.token_endpoint,
        remote_client_id: record.remote_client_id,
        has_client_secret: record.client_secret.is_some(),
        token_endpoint_auth_method: api_token_endpoint_auth_method(
            record.token_endpoint_auth_method,
        ),
        scopes_default: record.scopes_default,
        audience: record.audience,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn auth_flow_view(
    record: auth_registry::AuthFlowRecord,
    now_ms: i64,
) -> api::AuthFlowView {
    api::AuthFlowView {
        flow_id: record.flow_id.as_str().to_owned(),
        client_id: record.client_id.as_str().to_owned(),
        provider_id: record.provider_id.clone(),
        status: api_auth_flow_status(record.status(now_ms)),
        grant_id: record
            .grant_id
            .as_ref()
            .map(|grant_id| grant_id.as_str().to_owned()),
        error: record.error.clone(),
        expires_at_ms: record.expires_at_ms,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn registry_auth_provider_kind(
    value: api::AuthProviderKind,
) -> auth_registry::AuthProviderKind {
    match value {
        api::AuthProviderKind::StaticBearer => auth_registry::AuthProviderKind::StaticBearer,
        api::AuthProviderKind::McpOAuth => auth_registry::AuthProviderKind::McpOAuth,
        api::AuthProviderKind::GitHubApp => auth_registry::AuthProviderKind::GitHubApp,
        api::AuthProviderKind::GitHubAppUser => auth_registry::AuthProviderKind::GitHubAppUser,
        api::AuthProviderKind::GitHubOAuthApp => auth_registry::AuthProviderKind::GitHubOAuthApp,
        api::AuthProviderKind::CustomOAuth => auth_registry::AuthProviderKind::CustomOAuth,
        api::AuthProviderKind::ModelApiKey => auth_registry::AuthProviderKind::ModelApiKey,
        api::AuthProviderKind::ModelOAuth => auth_registry::AuthProviderKind::ModelOAuth,
    }
}

fn registry_token_endpoint_auth_method(
    value: api::TokenEndpointAuthMethod,
) -> auth_registry::TokenEndpointAuthMethod {
    match value {
        api::TokenEndpointAuthMethod::ClientSecretBasic => {
            auth_registry::TokenEndpointAuthMethod::ClientSecretBasic
        }
        api::TokenEndpointAuthMethod::ClientSecretPost => {
            auth_registry::TokenEndpointAuthMethod::ClientSecretPost
        }
        api::TokenEndpointAuthMethod::None => auth_registry::TokenEndpointAuthMethod::None,
    }
}

fn api_token_endpoint_auth_method(
    value: auth_registry::TokenEndpointAuthMethod,
) -> api::TokenEndpointAuthMethod {
    match value {
        auth_registry::TokenEndpointAuthMethod::ClientSecretBasic => {
            api::TokenEndpointAuthMethod::ClientSecretBasic
        }
        auth_registry::TokenEndpointAuthMethod::ClientSecretPost => {
            api::TokenEndpointAuthMethod::ClientSecretPost
        }
        auth_registry::TokenEndpointAuthMethod::None => api::TokenEndpointAuthMethod::None,
    }
}

fn api_auth_flow_status(value: auth_registry::AuthFlowStatus) -> api::AuthFlowStatus {
    match value {
        auth_registry::AuthFlowStatus::Pending => api::AuthFlowStatus::Pending,
        auth_registry::AuthFlowStatus::Completed => api::AuthFlowStatus::Completed,
        auth_registry::AuthFlowStatus::Failed => api::AuthFlowStatus::Failed,
        auth_registry::AuthFlowStatus::Expired => api::AuthFlowStatus::Expired,
    }
}

/// The gateway-hosted redirect URI for authorization callbacks.
pub(super) fn oauth_redirect_uri(public_base_url: &str) -> String {
    format!("{}/auth/callback", public_base_url.trim_end_matches('/'))
}

/// The gateway-hosted Client ID Metadata Document URL. CIMD client ids must
/// be HTTPS URLs the authorization server can fetch, so this is only usable
/// when the deployment has a public https base URL.
pub(super) fn cimd_client_id_url(public_base_url: &str) -> String {
    format!(
        "{}/auth/client-metadata.json",
        public_base_url.trim_end_matches('/')
    )
}

pub(super) fn cimd_config(public_base_url: &str) -> Option<auth_registry::CimdConfig> {
    public_base_url
        .starts_with("https://")
        .then(|| auth_registry::CimdConfig {
            client_id_url: cimd_client_id_url(public_base_url),
        })
}

/// The Client ID Metadata Document this gateway serves
/// (draft-ietf-oauth-client-id-metadata-document): a public PKCE client
/// whose id is the document URL itself.
pub(super) fn cimd_document(public_base_url: &str) -> serde_json::Value {
    let base = public_base_url.trim_end_matches('/');
    serde_json::json!({
        "client_id": cimd_client_id_url(public_base_url),
        "client_name": "Lightspeed",
        "client_uri": base,
        "redirect_uris": [oauth_redirect_uri(public_base_url)],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    })
}

/// Build the discovery target for an OAuth-protected MCP server from its
/// catalog record. Bearer/no-auth servers cannot be logged into.
pub(super) fn mcp_oauth_target_from_record(
    record: &mcp_registry::McpServerRecord,
) -> Result<auth_registry::McpOAuthTarget, AgentApiError> {
    match &record.auth_policy {
        mcp_registry::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        }
        | mcp_registry::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => Ok(auth_registry::McpOAuthTarget {
            server_id: record.server_id.as_str().to_owned(),
            server_url: resource.clone(),
            scopes_default: scopes_default.clone(),
            protected_resource_metadata_url: protected_resource_metadata_url.clone(),
            authorization_server_hint: authorization_server.clone(),
        }),
        other => Err(AgentApiError::rejected(format!(
            "MCP server {} auth policy {:?} does not use OAuth; use `auth grant import` \
             for bearer servers",
            record.server_id, other
        ))),
    }
}

pub(super) fn map_mcp_oauth_error(error: auth_registry::McpOAuthError) -> AgentApiError {
    match error {
        auth_registry::McpOAuthError::Registry(error) => map_auth_registry_error(error),
        other => AgentApiError::rejected(other.to_string()),
    }
}
