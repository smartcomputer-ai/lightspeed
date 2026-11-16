//! OAuth client and authorization flow API helpers.
//!
//! Maps between `api` DTOs and `auth` records. The client secret in
//! `credentials/clients/create` params is the second deliberate inbound-plaintext
//! path: it is drafted into an encrypted secret record here and never appears
//! in views or logs.

use super::*;

pub(super) fn parse_oauth_client_id(
    client_id: String,
) -> Result<credentials::OAuthClientId, AgentApiError> {
    credentials::OAuthClientId::try_new(client_id).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid oauth client id: {error}"))
    })
}

pub(super) fn parse_credential_flow_id(
    flow_id: String,
) -> Result<credentials::CredentialFlowId, AgentApiError> {
    credentials::CredentialFlowId::try_new(flow_id).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid credential flow id: {error}"))
    })
}

#[derive(Debug)]
pub(super) struct CredentialClientCreateDraft {
    pub(super) secret: Option<credentials::PutSecretRecord>,
    pub(super) client: credentials::CreateOAuthClientRecord,
}

pub(super) fn credential_client_create_draft(
    params: CredentialClientCreateParams,
    now_ms: i64,
) -> Result<CredentialClientCreateDraft, AgentApiError> {
    let client_id = match params.client_id {
        Some(client_id) => parse_oauth_client_id(client_id)?,
        None => {
            credentials::OAuthClientId::try_new(credentials::random_credential_id("authclient_"))
                .map_err(|error| {
                    AgentApiError::internal(format!("generate oauth client id: {error}"))
                })?
        }
    };
    let auth_method = params.token_endpoint_auth_method.map_or_else(
        || {
            if params.client_secret.is_some() {
                credentials::TokenEndpointAuthMethod::ClientSecretBasic
            } else {
                credentials::TokenEndpointAuthMethod::None
            }
        },
        registry_token_endpoint_auth_method,
    );
    let secret = params
        .client_secret
        .map(|client_secret| {
            let secret_id =
                credentials::SecretId::try_new(credentials::random_credential_id("authsec_"))
                    .map_err(|error| {
                        AgentApiError::internal(format!("generate secret id: {error}"))
                    })?;
            Ok::<_, AgentApiError>(credentials::PutSecretRecord {
                secret_id,
                secret_kind: credentials::SECRET_KIND_OAUTH_CLIENT_SECRET.to_owned(),
                value: credentials::SecretValue::new(client_secret),
                created_at_ms: now_ms,
            })
        })
        .transpose()?;

    let client = credentials::CreateOAuthClientRecord {
        client_id: client_id.clone(),
        provider_id: params
            .provider_id
            .unwrap_or_else(|| client_id.as_str().to_owned()),
        provider_kind: registry_credential_provider_kind(params.provider_kind),
        display_name: params.display_name,
        authorization_endpoint: params.authorization_endpoint,
        token_endpoint: params.token_endpoint,
        remote_client_id: params.remote_client_id,
        client_secret: secret.as_ref().map(|secret| secret.secret_id.clone()),
        token_endpoint_auth_method: auth_method,
        scopes_default: params.scopes_default,
        audience: params.audience,
        authorization_server_issuer: params.authorization_server_issuer,
        authorization_response_iss_parameter_supported: params
            .authorization_response_iss_parameter_supported,
        authorization_server_scopes_supported: params.authorization_server_scopes_supported,
        created_at_ms: now_ms,
    };
    if let Some(secret) = &secret {
        secret.validate().map_err(map_credential_error)?;
    }
    client
        .clone()
        .into_record()
        .validate()
        .map_err(map_credential_error)?;
    Ok(CredentialClientCreateDraft { secret, client })
}

pub(super) fn oauth_client_view(record: credentials::OAuthClientRecord) -> api::OAuthClientView {
    api::OAuthClientView {
        client_id: record.client_id.as_str().to_owned(),
        provider_id: record.provider_id,
        provider_kind: api_credential_provider_kind(record.provider_kind),
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
        authorization_server_issuer: record.authorization_server_issuer,
        authorization_response_iss_parameter_supported: record
            .authorization_response_iss_parameter_supported,
        authorization_server_scopes_supported: record.authorization_server_scopes_supported,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn credential_flow_view(
    record: credentials::CredentialFlowRecord,
    now_ms: i64,
) -> api::CredentialFlowView {
    api::CredentialFlowView {
        flow_id: record.flow_id.as_str().to_owned(),
        client_id: record.client_id.as_str().to_owned(),
        provider_id: record.provider_id.clone(),
        status: api_credential_flow_status(record.status(now_ms)),
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

pub(super) fn registry_credential_provider_kind(
    value: api::CredentialProviderKind,
) -> credentials::CredentialProviderKind {
    match value {
        api::CredentialProviderKind::StaticToken => {
            credentials::CredentialProviderKind::StaticToken
        }
        api::CredentialProviderKind::McpOAuth => credentials::CredentialProviderKind::McpOAuth,
        api::CredentialProviderKind::GitHubApp => credentials::CredentialProviderKind::GitHubApp,
        api::CredentialProviderKind::CustomOAuth => {
            credentials::CredentialProviderKind::CustomOAuth
        }
        api::CredentialProviderKind::ModelApiKey => {
            credentials::CredentialProviderKind::ModelApiKey
        }
        api::CredentialProviderKind::ModelOAuth => credentials::CredentialProviderKind::ModelOAuth,
        api::CredentialProviderKind::ModelEndpoint => {
            credentials::CredentialProviderKind::ModelEndpoint
        }
    }
}

fn registry_token_endpoint_auth_method(
    value: api::TokenEndpointAuthMethod,
) -> credentials::TokenEndpointAuthMethod {
    match value {
        api::TokenEndpointAuthMethod::ClientSecretBasic => {
            credentials::TokenEndpointAuthMethod::ClientSecretBasic
        }
        api::TokenEndpointAuthMethod::ClientSecretPost => {
            credentials::TokenEndpointAuthMethod::ClientSecretPost
        }
        api::TokenEndpointAuthMethod::None => credentials::TokenEndpointAuthMethod::None,
    }
}

fn api_token_endpoint_auth_method(
    value: credentials::TokenEndpointAuthMethod,
) -> api::TokenEndpointAuthMethod {
    match value {
        credentials::TokenEndpointAuthMethod::ClientSecretBasic => {
            api::TokenEndpointAuthMethod::ClientSecretBasic
        }
        credentials::TokenEndpointAuthMethod::ClientSecretPost => {
            api::TokenEndpointAuthMethod::ClientSecretPost
        }
        credentials::TokenEndpointAuthMethod::None => api::TokenEndpointAuthMethod::None,
    }
}

fn api_credential_flow_status(
    value: credentials::CredentialFlowStatus,
) -> api::CredentialFlowStatus {
    match value {
        credentials::CredentialFlowStatus::Pending => api::CredentialFlowStatus::Pending,
        credentials::CredentialFlowStatus::Completed => api::CredentialFlowStatus::Completed,
        credentials::CredentialFlowStatus::Failed => api::CredentialFlowStatus::Failed,
        credentials::CredentialFlowStatus::Expired => api::CredentialFlowStatus::Expired,
    }
}

/// The gateway-hosted redirect URI for authorization callbacks.
pub(super) fn oauth_redirect_uri(public_base_url: &str) -> String {
    format!(
        "{}/credentials/callback",
        public_base_url.trim_end_matches('/')
    )
}

/// The gateway-hosted Client ID Metadata Document URL. CIMD client ids must
/// be HTTPS URLs the authorization server can fetch, so this is only usable
/// when the deployment has a public https base URL.
pub(super) fn cimd_client_id_url(public_base_url: &str) -> String {
    format!(
        "{}/credentials/client-metadata.json",
        public_base_url.trim_end_matches('/')
    )
}

pub(super) fn cimd_config(public_base_url: &str) -> Option<credentials::CimdConfig> {
    public_base_url
        .starts_with("https://")
        .then(|| credentials::CimdConfig {
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
    record: &mcp::McpServerRecord,
) -> Result<credentials::McpOAuthTarget, AgentApiError> {
    match &record.auth_policy {
        mcp::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        }
        | mcp::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => Ok(credentials::McpOAuthTarget {
            server_id: record.server_id.as_str().to_owned(),
            server_url: resource.clone(),
            scopes_default: scopes_default.clone(),
            protected_resource_metadata_url: protected_resource_metadata_url.clone(),
            authorization_server_hint: authorization_server.clone(),
        }),
        other => Err(AgentApiError::rejected(format!(
            "MCP server {} auth policy {:?} does not use OAuth; use `credential grant import` \
             for bearer servers",
            record.server_id, other
        ))),
    }
}

pub(super) fn map_mcp_oauth_error(error: credentials::McpOAuthError) -> AgentApiError {
    match error {
        credentials::McpOAuthError::Registry(error) => map_credential_error(error),
        other => AgentApiError::rejected(other.to_string()),
    }
}
