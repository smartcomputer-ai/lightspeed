use super::*;

pub(super) fn parse_auth_grant_id(grant_id: String) -> Result<auth::AuthGrantId, AgentApiError> {
    auth::AuthGrantId::try_new(grant_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid auth grant id: {error}")))
}

pub(super) struct AuthGrantImportDraft {
    pub(super) secret: auth::PutSecretRecord,
    pub(super) grant: auth::CreateAuthGrantRecord,
}

pub(super) fn auth_grant_import_draft(
    params: AuthGrantImportParams,
    now_ms: i64,
) -> Result<AuthGrantImportDraft, AgentApiError> {
    let grant_id = match params.grant_id {
        Some(grant_id) => parse_auth_grant_id(grant_id)?,
        None => auth::AuthGrantId::try_new(format!("authgrant_{}", uuid::Uuid::new_v4().simple()))
            .map_err(|error| AgentApiError::internal(format!("generate auth grant id: {error}")))?,
    };
    let secret_id =
        auth::SecretId::try_new(format!("authsec_{}", uuid::Uuid::new_v4().simple()))
            .map_err(|error| AgentApiError::internal(format!("generate secret id: {error}")))?;

    let secret = auth::PutSecretRecord {
        secret_id: secret_id.clone(),
        secret_kind: auth::SECRET_KIND_STATIC_BEARER.to_owned(),
        value: auth::SecretValue::new(params.token),
        created_at_ms: now_ms,
    };
    let grant = auth::CreateAuthGrantRecord {
        grant_id,
        provider_id: params.provider_id.unwrap_or_else(|| "static".to_owned()),
        provider_kind: auth::AuthProviderKind::StaticBearer,
        exposure: registry_auth_grant_exposure(params.exposure),
        principal: crate::gateway::principal::request_principal(),
        display_name: params.display_name,
        subject_hint: params.subject_hint,
        scopes: params.scopes,
        audience: params.audience,
        access_token_secret: Some(secret_id),
        refresh_token_secret: None,
        oauth_client: None,
        metadata: params
            .metadata
            .unwrap_or_else(|| serde_json::Value::Object(Default::default())),
        expires_at_ms: params.expires_at_ms,
        status: auth::AuthGrantStatus::Active,
        created_at_ms: now_ms,
    };
    secret.validate().map_err(map_auth_error)?;
    grant
        .clone()
        .into_record()
        .validate()
        .map_err(map_auth_error)?;
    Ok(AuthGrantImportDraft { secret, grant })
}

pub(super) fn auth_grant_view(record: auth::AuthGrantRecord) -> api::AuthGrantView {
    api::AuthGrantView {
        grant_id: record.grant_id.as_str().to_owned(),
        provider_id: record.provider_id,
        provider_kind: api_auth_provider_kind(record.provider_kind),
        exposure: api_auth_grant_exposure(record.exposure),
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
        last_leased_at_ms: record.last_leased_at_ms,
        lease_count: record.lease_count,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn require_retrievable_grant(
    grant: &auth::AuthGrantRecord,
) -> Result<(), AgentApiError> {
    if grant.exposure == auth::AuthGrantExposure::Retrievable {
        Ok(())
    } else {
        Err(AgentApiError::rejected(format!(
            "auth grant {} is brokered and cannot be leased",
            grant.grant_id
        )))
    }
}

pub(super) fn map_auth_broker_error(error: auth::AuthBrokerError) -> AgentApiError {
    match error {
        auth::AuthBrokerError::GrantNotFound { grant_id } => {
            AgentApiError::not_found(format!("auth grant not found: {grant_id}"))
        }
        auth::AuthBrokerError::GrantNotActive { .. }
        | auth::AuthBrokerError::GrantExpired { .. }
        | auth::AuthBrokerError::AudienceMismatch { .. }
        | auth::AuthBrokerError::SecretMissing { .. }
        | auth::AuthBrokerError::RefreshFailed { .. }
        | auth::AuthBrokerError::MintFailed { .. }
        | auth::AuthBrokerError::SourceNotConfigured { .. } => {
            AgentApiError::rejected(error.to_string())
        }
        auth::AuthBrokerError::Store { message } => AgentApiError::internal(message),
    }
}

pub(crate) fn map_auth_error(error: auth::AuthRegistryError) -> AgentApiError {
    match error {
        auth::AuthRegistryError::GrantAlreadyExists { grant_id } => {
            AgentApiError::conflict(format!("auth grant already exists: {grant_id}"))
        }
        auth::AuthRegistryError::GrantNotFound { grant_id } => {
            AgentApiError::not_found(format!("auth grant not found: {grant_id}"))
        }
        auth::AuthRegistryError::SecretAlreadyExists { secret_id } => {
            AgentApiError::conflict(format!("secret already exists: {secret_id}"))
        }
        auth::AuthRegistryError::SecretNotFound { secret_id } => {
            AgentApiError::not_found(format!("secret not found: {secret_id}"))
        }
        auth::AuthRegistryError::ClientAlreadyExists { client_id } => {
            AgentApiError::conflict(format!("oauth client already exists: {client_id}"))
        }
        auth::AuthRegistryError::ClientNotFound { client_id } => {
            AgentApiError::not_found(format!("oauth client not found: {client_id}"))
        }
        auth::AuthRegistryError::ProviderAlreadyExists { provider_id } => {
            AgentApiError::conflict(format!("auth provider already exists: {provider_id}"))
        }
        auth::AuthRegistryError::ProviderNotFound { provider_id } => {
            AgentApiError::not_found(format!("auth provider not found: {provider_id}"))
        }
        auth::AuthRegistryError::FlowAlreadyExists { flow_id } => {
            AgentApiError::conflict(format!("auth flow already exists: {flow_id}"))
        }
        auth::AuthRegistryError::FlowNotFound { flow_id } => {
            AgentApiError::not_found(format!("auth flow not found: {flow_id}"))
        }
        auth::AuthRegistryError::FlowAlreadyConsumed { flow_id } => {
            AgentApiError::conflict(format!("auth flow was already consumed: {flow_id}"))
        }
        auth::AuthRegistryError::FlowAlreadyCompleted { flow_id } => {
            AgentApiError::conflict(format!("auth flow was already completed: {flow_id}"))
        }
        auth::AuthRegistryError::FlowExpired { flow_id } => {
            AgentApiError::rejected(format!("auth flow is expired: {flow_id}"))
        }
        auth::AuthRegistryError::UnknownCallbackState => {
            AgentApiError::rejected("authorization callback state is unknown or no longer valid")
        }
        auth::AuthRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        auth::AuthRegistryError::Store { message } => AgentApiError::internal(message),
    }
}

/// MCP-specific grant compatibility for a universe server credential: the grant
/// must be active, its provider-kind class must match the server auth policy,
/// and its audience (when bound) must cover the server URL and configured OAuth
/// resource. Universe equality holds by construction: the gateway's grant and
/// catalog stores are bound to the same universe.
pub(super) fn validate_mcp_grant_for_server(
    record: &mcp::McpServerRecord,
    grant: &auth::AuthGrantRecord,
) -> Result<(), AgentApiError> {
    if grant.status != auth::AuthGrantStatus::Active {
        return Err(AgentApiError::rejected(format!(
            "auth grant {} is not active: {:?}",
            grant.grant_id, grant.status
        )));
    }

    let kind_compatible = match &record.auth_policy {
        mcp::McpServerAuthPolicy::None => false,
        mcp::McpServerAuthPolicy::OptionalBearer | mcp::McpServerAuthPolicy::RequiredBearer => {
            grant.provider_kind == auth::AuthProviderKind::StaticBearer
        }
        mcp::McpServerAuthPolicy::OptionalOAuth { .. }
        | mcp::McpServerAuthPolicy::RequiredOAuth { .. } => {
            grant.provider_kind == auth::AuthProviderKind::McpOAuth
        }
    };
    if !kind_compatible {
        return Err(AgentApiError::rejected(format!(
            "auth grant {} provider kind {:?} is not compatible with MCP server {} auth policy",
            grant.grant_id, grant.provider_kind, record.server_id
        )));
    }

    if let Some(audience) = &grant.audience {
        if !auth::audience_covers(audience, &record.server_url) {
            return Err(AgentApiError::rejected(format!(
                "auth grant {} audience does not cover MCP server URL {}",
                grant.grant_id, record.server_url
            )));
        }
        let oauth_resource = match &record.auth_policy {
            mcp::McpServerAuthPolicy::OptionalOAuth { resource, .. }
            | mcp::McpServerAuthPolicy::RequiredOAuth { resource, .. } => Some(resource),
            _ => None,
        };
        if let Some(resource) = oauth_resource
            && !auth::audience_covers(audience, resource)
        {
            return Err(AgentApiError::rejected(format!(
                "auth grant {} audience does not cover MCP OAuth resource {}",
                grant.grant_id, resource
            )));
        }
    }
    Ok(())
}

pub(super) fn api_auth_provider_kind(value: auth::AuthProviderKind) -> api::AuthProviderKind {
    match value {
        auth::AuthProviderKind::StaticBearer => api::AuthProviderKind::StaticBearer,
        auth::AuthProviderKind::McpOAuth => api::AuthProviderKind::McpOAuth,
        auth::AuthProviderKind::GitHubApp => api::AuthProviderKind::GitHubApp,
        auth::AuthProviderKind::CustomOAuth => api::AuthProviderKind::CustomOAuth,
        auth::AuthProviderKind::ModelApiKey => api::AuthProviderKind::ModelApiKey,
        auth::AuthProviderKind::ModelOAuth => api::AuthProviderKind::ModelOAuth,
        auth::AuthProviderKind::ModelEndpoint => api::AuthProviderKind::ModelEndpoint,
    }
}

fn api_auth_grant_exposure(value: auth::AuthGrantExposure) -> api::AuthGrantExposure {
    match value {
        auth::AuthGrantExposure::Brokered => api::AuthGrantExposure::Brokered,
        auth::AuthGrantExposure::Retrievable => api::AuthGrantExposure::Retrievable,
    }
}

pub(super) fn registry_auth_grant_exposure(
    value: api::AuthGrantExposure,
) -> auth::AuthGrantExposure {
    match value {
        api::AuthGrantExposure::Brokered => auth::AuthGrantExposure::Brokered,
        api::AuthGrantExposure::Retrievable => auth::AuthGrantExposure::Retrievable,
    }
}

fn api_principal_kind(value: auth::PrincipalKind) -> api::PrincipalKind {
    match value {
        auth::PrincipalKind::User => api::PrincipalKind::User,
        auth::PrincipalKind::ServiceAccount => api::PrincipalKind::ServiceAccount,
        auth::PrincipalKind::UniverseDefault => api::PrincipalKind::UniverseDefault,
    }
}

fn api_auth_grant_status(value: auth::AuthGrantStatus) -> api::AuthGrantStatus {
    match value {
        auth::AuthGrantStatus::Active => api::AuthGrantStatus::Active,
        auth::AuthGrantStatus::NeedsReauth => api::AuthGrantStatus::NeedsReauth,
        auth::AuthGrantStatus::Revoked => api::AuthGrantStatus::Revoked,
        auth::AuthGrantStatus::Failed => api::AuthGrantStatus::Failed,
    }
}

pub(super) fn registry_auth_grant_status_for_filter(
    value: api::AuthGrantStatus,
) -> auth::AuthGrantStatus {
    match value {
        api::AuthGrantStatus::Active => auth::AuthGrantStatus::Active,
        api::AuthGrantStatus::NeedsReauth => auth::AuthGrantStatus::NeedsReauth,
        api::AuthGrantStatus::Revoked => auth::AuthGrantStatus::Revoked,
        api::AuthGrantStatus::Failed => auth::AuthGrantStatus::Failed,
    }
}
