use super::*;

pub(super) fn parse_auth_grant_id(
    grant_id: String,
) -> Result<auth_registry::AuthGrantId, AgentApiError> {
    auth_registry::AuthGrantId::try_new(grant_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid auth grant id: {error}")))
}

pub(super) struct AuthGrantImportDraft {
    pub(super) secret: auth_registry::PutSecretRecord,
    pub(super) grant: auth_registry::CreateAuthGrantRecord,
}

pub(super) fn auth_grant_import_draft(
    params: AuthGrantImportParams,
    now_ms: i64,
) -> Result<AuthGrantImportDraft, AgentApiError> {
    let grant_id = match params.grant_id {
        Some(grant_id) => parse_auth_grant_id(grant_id)?,
        None => auth_registry::AuthGrantId::try_new(format!(
            "authgrant_{}",
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(|error| AgentApiError::internal(format!("generate auth grant id: {error}")))?,
    };
    let secret_id =
        auth_registry::SecretId::try_new(format!("authsec_{}", uuid::Uuid::new_v4().simple()))
            .map_err(|error| AgentApiError::internal(format!("generate secret id: {error}")))?;

    let secret = auth_registry::PutSecretRecord {
        secret_id: secret_id.clone(),
        secret_kind: auth_registry::SECRET_KIND_STATIC_BEARER.to_owned(),
        value: auth_registry::SecretValue::new(params.token),
        created_at_ms: now_ms,
    };
    let grant = auth_registry::CreateAuthGrantRecord {
        grant_id,
        provider_id: params.provider_id.unwrap_or_else(|| "static".to_owned()),
        provider_kind: auth_registry::AuthProviderKind::StaticBearer,
        principal: auth_registry::PrincipalRef::universe_default(),
        display_name: params.display_name,
        subject_hint: params.subject_hint,
        scopes: params.scopes,
        audience: params.audience,
        access_token_secret: Some(secret_id),
        refresh_token_secret: None,
        oauth_client: None,
        metadata: serde_json::Value::Object(Default::default()),
        expires_at_ms: params.expires_at_ms,
        status: auth_registry::AuthGrantStatus::Active,
        created_at_ms: now_ms,
    };
    secret.validate().map_err(map_auth_registry_error)?;
    grant
        .clone()
        .into_record()
        .validate()
        .map_err(map_auth_registry_error)?;
    Ok(AuthGrantImportDraft { secret, grant })
}

pub(super) fn auth_grant_view(record: auth_registry::AuthGrantRecord) -> api::AuthGrantView {
    api::AuthGrantView {
        grant_id: record.grant_id.as_str().to_owned(),
        provider_id: record.provider_id,
        provider_kind: api_auth_provider_kind(record.provider_kind),
        principal: api::PrincipalRefView {
            kind: api_principal_kind(record.principal.kind),
            id: record.principal.id,
        },
        display_name: record.display_name,
        subject_hint: record.subject_hint,
        scopes: record.scopes,
        audience: record.audience,
        has_access_token: record.access_token_secret.is_some(),
        has_refresh_token: record.refresh_token_secret.is_some(),
        expires_at_ms: record.expires_at_ms,
        status: api_auth_grant_status(record.status),
        metadata: record.metadata,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn map_auth_registry_error(error: auth_registry::AuthRegistryError) -> AgentApiError {
    match error {
        auth_registry::AuthRegistryError::GrantAlreadyExists { grant_id } => {
            AgentApiError::conflict(format!("auth grant already exists: {grant_id}"))
        }
        auth_registry::AuthRegistryError::GrantNotFound { grant_id } => {
            AgentApiError::not_found(format!("auth grant not found: {grant_id}"))
        }
        auth_registry::AuthRegistryError::SecretAlreadyExists { secret_id } => {
            AgentApiError::conflict(format!("secret already exists: {secret_id}"))
        }
        auth_registry::AuthRegistryError::SecretNotFound { secret_id } => {
            AgentApiError::not_found(format!("secret not found: {secret_id}"))
        }
        auth_registry::AuthRegistryError::ClientAlreadyExists { client_id } => {
            AgentApiError::conflict(format!("oauth client already exists: {client_id}"))
        }
        auth_registry::AuthRegistryError::ClientNotFound { client_id } => {
            AgentApiError::not_found(format!("oauth client not found: {client_id}"))
        }
        auth_registry::AuthRegistryError::ProviderAlreadyExists { provider_id } => {
            AgentApiError::conflict(format!("auth provider already exists: {provider_id}"))
        }
        auth_registry::AuthRegistryError::ProviderNotFound { provider_id } => {
            AgentApiError::not_found(format!("auth provider not found: {provider_id}"))
        }
        auth_registry::AuthRegistryError::FlowAlreadyExists { flow_id } => {
            AgentApiError::conflict(format!("auth flow already exists: {flow_id}"))
        }
        auth_registry::AuthRegistryError::FlowNotFound { flow_id } => {
            AgentApiError::not_found(format!("auth flow not found: {flow_id}"))
        }
        auth_registry::AuthRegistryError::FlowAlreadyConsumed { flow_id } => {
            AgentApiError::conflict(format!("auth flow was already consumed: {flow_id}"))
        }
        auth_registry::AuthRegistryError::FlowAlreadyCompleted { flow_id } => {
            AgentApiError::conflict(format!("auth flow was already completed: {flow_id}"))
        }
        auth_registry::AuthRegistryError::FlowExpired { flow_id } => {
            AgentApiError::rejected(format!("auth flow is expired: {flow_id}"))
        }
        auth_registry::AuthRegistryError::UnknownCallbackState => {
            AgentApiError::rejected("authorization callback state is unknown or no longer valid")
        }
        auth_registry::AuthRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        auth_registry::AuthRegistryError::Store { message } => AgentApiError::internal(message),
    }
}

/// MCP-specific grant compatibility for session linking (P68 G2): the grant
/// must be active, its provider-kind class must match the server auth policy,
/// and its audience (when bound) must cover the server URL. Universe equality
/// holds by construction: the gateway's grant and catalog stores are bound to
/// the same universe.
pub(super) fn validate_mcp_grant_for_link(
    record: &mcp_registry::McpServerRecord,
    grant: &auth_registry::AuthGrantRecord,
) -> Result<(), AgentApiError> {
    if grant.status != auth_registry::AuthGrantStatus::Active {
        return Err(AgentApiError::rejected(format!(
            "auth grant {} is not active: {:?}",
            grant.grant_id, grant.status
        )));
    }

    let kind_compatible = match &record.auth_policy {
        mcp_registry::McpServerAuthPolicy::None => false,
        mcp_registry::McpServerAuthPolicy::OptionalBearer
        | mcp_registry::McpServerAuthPolicy::RequiredBearer => {
            grant.provider_kind == auth_registry::AuthProviderKind::StaticBearer
        }
        mcp_registry::McpServerAuthPolicy::OptionalOAuth { .. }
        | mcp_registry::McpServerAuthPolicy::RequiredOAuth { .. } => {
            grant.provider_kind == auth_registry::AuthProviderKind::McpOAuth
        }
    };
    if !kind_compatible {
        return Err(AgentApiError::rejected(format!(
            "auth grant {} provider kind {:?} is not compatible with MCP server {} auth policy",
            grant.grant_id, grant.provider_kind, record.server_id
        )));
    }

    if let Some(audience) = &grant.audience {
        if !auth_registry::audience_covers(audience, &record.server_url) {
            return Err(AgentApiError::rejected(format!(
                "auth grant {} audience does not cover MCP server URL {}",
                grant.grant_id, record.server_url
            )));
        }
    }
    Ok(())
}

pub(super) fn api_auth_provider_kind(
    value: auth_registry::AuthProviderKind,
) -> api::AuthProviderKind {
    match value {
        auth_registry::AuthProviderKind::StaticBearer => api::AuthProviderKind::StaticBearer,
        auth_registry::AuthProviderKind::McpOAuth => api::AuthProviderKind::McpOAuth,
        auth_registry::AuthProviderKind::GitHubApp => api::AuthProviderKind::GitHubApp,
        auth_registry::AuthProviderKind::GitHubAppUser => api::AuthProviderKind::GitHubAppUser,
        auth_registry::AuthProviderKind::GitHubOAuthApp => api::AuthProviderKind::GitHubOAuthApp,
        auth_registry::AuthProviderKind::CustomOAuth => api::AuthProviderKind::CustomOAuth,
    }
}

fn api_principal_kind(value: auth_registry::PrincipalKind) -> api::PrincipalKind {
    match value {
        auth_registry::PrincipalKind::User => api::PrincipalKind::User,
        auth_registry::PrincipalKind::ServiceAccount => api::PrincipalKind::ServiceAccount,
        auth_registry::PrincipalKind::UniverseDefault => api::PrincipalKind::UniverseDefault,
    }
}

fn api_auth_grant_status(value: auth_registry::AuthGrantStatus) -> api::AuthGrantStatus {
    match value {
        auth_registry::AuthGrantStatus::Active => api::AuthGrantStatus::Active,
        auth_registry::AuthGrantStatus::NeedsReauth => api::AuthGrantStatus::NeedsReauth,
        auth_registry::AuthGrantStatus::Revoked => api::AuthGrantStatus::Revoked,
        auth_registry::AuthGrantStatus::Failed => api::AuthGrantStatus::Failed,
    }
}

pub(super) fn registry_auth_grant_status_for_filter(
    value: api::AuthGrantStatus,
) -> auth_registry::AuthGrantStatus {
    match value {
        api::AuthGrantStatus::Active => auth_registry::AuthGrantStatus::Active,
        api::AuthGrantStatus::NeedsReauth => auth_registry::AuthGrantStatus::NeedsReauth,
        api::AuthGrantStatus::Revoked => auth_registry::AuthGrantStatus::Revoked,
        api::AuthGrantStatus::Failed => auth_registry::AuthGrantStatus::Failed,
    }
}
