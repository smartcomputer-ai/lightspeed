use super::*;

pub(super) fn parse_credential_grant_id(
    grant_id: String,
) -> Result<credentials::CredentialGrantId, AgentApiError> {
    credentials::CredentialGrantId::try_new(grant_id).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid credential grant id: {error}"))
    })
}

pub(super) struct CredentialGrantImportDraft {
    pub(super) secret: credentials::PutSecretRecord,
    pub(super) grant: credentials::CreateCredentialGrantRecord,
}

pub(super) fn credential_grant_import_draft(
    params: CredentialGrantImportParams,
    now_ms: i64,
) -> Result<CredentialGrantImportDraft, AgentApiError> {
    let grant_id = match params.grant_id {
        Some(grant_id) => parse_credential_grant_id(grant_id)?,
        None => credentials::CredentialGrantId::try_new(format!(
            "authgrant_{}",
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(|error| {
            AgentApiError::internal(format!("generate credential grant id: {error}"))
        })?,
    };
    let secret_id =
        credentials::SecretId::try_new(format!("authsec_{}", uuid::Uuid::new_v4().simple()))
            .map_err(|error| AgentApiError::internal(format!("generate secret id: {error}")))?;

    let secret = credentials::PutSecretRecord {
        secret_id: secret_id.clone(),
        secret_kind: credentials::SECRET_KIND_STATIC_TOKEN.to_owned(),
        value: credentials::SecretValue::new(params.token),
        created_at_ms: now_ms,
    };
    let grant = credentials::CreateCredentialGrantRecord {
        grant_id,
        provider_id: params.provider_id.unwrap_or_else(|| "static".to_owned()),
        provider_kind: credentials::CredentialProviderKind::StaticToken,
        exposure: registry_credential_grant_exposure(params.exposure),
        created_by: crate::gateway::request_context::request_attribution()?,
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
        status: credentials::CredentialGrantStatus::Active,
        created_at_ms: now_ms,
    };
    secret.validate().map_err(map_credential_error)?;
    grant
        .clone()
        .into_record()
        .validate()
        .map_err(map_credential_error)?;
    Ok(CredentialGrantImportDraft { secret, grant })
}

/// What a declaration writes over the token credential that exists: the
/// secret it is expected to still point at, and its declared settings (still
/// pointing at that secret; the caller swaps in a new one when the token
/// differs). Refuses what a declaration never changes: another kind of grant,
/// so OAuth, GitHub App and model grants keep their own refresh and
/// reconnect paths; a credential that is not active, so a revoked one stays
/// revoked; and a different provider label or exposure.
pub(super) fn token_credential_update(
    current: &credentials::CredentialGrantRecord,
    draft: &CredentialGrantImportDraft,
    now_ms: i64,
) -> Result<(credentials::SecretId, credentials::TokenCredentialUpdate), AgentApiError> {
    let grant_id = &current.grant_id;
    if current.provider_kind != credentials::CredentialProviderKind::StaticToken {
        return Err(AgentApiError::conflict(format!(
            "credential grant {grant_id} is a {:?} credential; only token credentials can be declared",
            current.provider_kind
        )));
    }
    if current.status != credentials::CredentialGrantStatus::Active {
        return Err(AgentApiError::conflict(format!(
            "credential grant {grant_id} is not active ({:?}); declare the credential under another id",
            current.status
        )));
    }
    if current.exposure != draft.grant.exposure {
        return Err(AgentApiError::conflict(format!(
            "credential grant {grant_id} already exists and its exposure cannot change; declare a new credential"
        )));
    }
    if current.provider_id != draft.grant.provider_id {
        return Err(AgentApiError::conflict(format!(
            "credential grant {grant_id} already exists under provider {}; declare a new credential",
            current.provider_id
        )));
    }
    let Some(expected) = current.access_token_secret.clone() else {
        return Err(AgentApiError::internal(format!(
            "token credential {grant_id} has no stored value"
        )));
    };
    let update = credentials::TokenCredentialUpdate {
        access_token_secret: expected.clone(),
        display_name: draft.grant.display_name.clone(),
        subject_hint: draft.grant.subject_hint.clone(),
        scopes: draft.grant.scopes.clone(),
        audience: draft.grant.audience.clone(),
        metadata: draft.grant.metadata.clone(),
        expires_at_ms: draft.grant.expires_at_ms,
        updated_at_ms: now_ms,
    };
    Ok((expected, update))
}

pub(super) fn credential_grant_view(
    record: credentials::CredentialGrantRecord,
) -> api::CredentialGrantView {
    api::CredentialGrantView {
        grant_id: record.grant_id.as_str().to_owned(),
        provider_id: record.provider_id,
        provider_kind: api_credential_provider_kind(record.provider_kind),
        exposure: api_credential_grant_exposure(record.exposure),
        created_by: record.created_by,
        display_name: record.display_name,
        subject_hint: record.subject_hint,
        scopes: record.scopes,
        audience: record.audience,
        has_access_token: record.access_token_secret.is_some(),
        has_refresh_token: record.refresh_token_secret.is_some(),
        expires_at_ms: record.expires_at_ms,
        status: api_credential_grant_status(record.status),
        metadata: record.metadata,
        last_leased_at_ms: record.last_leased_at_ms,
        lease_count: record.lease_count,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn require_retrievable_grant(
    grant: &credentials::CredentialGrantRecord,
) -> Result<(), AgentApiError> {
    if grant.exposure == credentials::CredentialGrantExposure::Retrievable {
        Ok(())
    } else {
        Err(AgentApiError::rejected(format!(
            "credential grant {} is brokered and cannot be leased",
            grant.grant_id
        )))
    }
}

pub(super) fn map_credential_broker_error(
    error: credentials::CredentialBrokerError,
) -> AgentApiError {
    match error {
        credentials::CredentialBrokerError::GrantNotFound { grant_id } => {
            AgentApiError::not_found(format!("credential grant not found: {grant_id}"))
        }
        credentials::CredentialBrokerError::GrantNotActive { .. }
        | credentials::CredentialBrokerError::GrantExpired { .. }
        | credentials::CredentialBrokerError::AudienceMismatch { .. }
        | credentials::CredentialBrokerError::SecretMissing { .. }
        | credentials::CredentialBrokerError::RefreshFailed { .. }
        | credentials::CredentialBrokerError::MintFailed { .. }
        | credentials::CredentialBrokerError::SourceNotConfigured { .. } => {
            AgentApiError::rejected(error.to_string())
        }
        credentials::CredentialBrokerError::Store { message } => AgentApiError::internal(message),
    }
}

pub(crate) fn map_credential_error(error: credentials::CredentialRegistryError) -> AgentApiError {
    match error {
        credentials::CredentialRegistryError::GrantAlreadyExists { grant_id } => {
            AgentApiError::conflict(format!("credential grant already exists: {grant_id}"))
        }
        credentials::CredentialRegistryError::GrantNotFound { grant_id } => {
            AgentApiError::not_found(format!("credential grant not found: {grant_id}"))
        }
        credentials::CredentialRegistryError::SecretAlreadyExists { secret_id } => {
            AgentApiError::conflict(format!("secret already exists: {secret_id}"))
        }
        credentials::CredentialRegistryError::SecretNotFound { secret_id } => {
            AgentApiError::not_found(format!("secret not found: {secret_id}"))
        }
        credentials::CredentialRegistryError::ClientAlreadyExists { client_id } => {
            AgentApiError::conflict(format!("oauth client already exists: {client_id}"))
        }
        credentials::CredentialRegistryError::ClientNotFound { client_id } => {
            AgentApiError::not_found(format!("oauth client not found: {client_id}"))
        }
        credentials::CredentialRegistryError::ProviderAlreadyExists { provider_id } => {
            AgentApiError::conflict(format!("credential provider already exists: {provider_id}"))
        }
        credentials::CredentialRegistryError::ProviderNotFound { provider_id } => {
            AgentApiError::not_found(format!("credential provider not found: {provider_id}"))
        }
        credentials::CredentialRegistryError::FlowAlreadyExists { flow_id } => {
            AgentApiError::conflict(format!("credential flow already exists: {flow_id}"))
        }
        credentials::CredentialRegistryError::FlowNotFound { flow_id } => {
            AgentApiError::not_found(format!("credential flow not found: {flow_id}"))
        }
        credentials::CredentialRegistryError::FlowAlreadyConsumed { flow_id } => {
            AgentApiError::conflict(format!("credential flow was already consumed: {flow_id}"))
        }
        credentials::CredentialRegistryError::FlowAlreadyCompleted { flow_id } => {
            AgentApiError::conflict(format!("credential flow was already completed: {flow_id}"))
        }
        credentials::CredentialRegistryError::FlowExpired { flow_id } => {
            AgentApiError::rejected(format!("credential flow is expired: {flow_id}"))
        }
        credentials::CredentialRegistryError::UnknownCallbackState => {
            AgentApiError::rejected("authorization callback state is unknown or no longer valid")
        }
        credentials::CredentialRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        credentials::CredentialRegistryError::Store { message } => AgentApiError::internal(message),
    }
}

/// MCP-specific grant compatibility for a universe server credential: the grant
/// must be active, its provider-kind class must match the server auth policy,
/// and its audience (when bound) must cover the server URL and configured OAuth
/// resource. Universe equality holds by construction: the gateway's grant and
/// catalog stores are bound to the same universe.
pub(super) fn validate_mcp_grant_for_server(
    record: &mcp::McpServerRecord,
    grant: &credentials::CredentialGrantRecord,
) -> Result<(), AgentApiError> {
    if grant.status != credentials::CredentialGrantStatus::Active {
        return Err(AgentApiError::rejected(format!(
            "credential grant {} is not active: {:?}",
            grant.grant_id, grant.status
        )));
    }

    let kind_compatible = match &record.auth_policy {
        mcp::McpServerAuthPolicy::None => false,
        mcp::McpServerAuthPolicy::OptionalBearer | mcp::McpServerAuthPolicy::RequiredBearer => {
            grant.provider_kind == credentials::CredentialProviderKind::StaticToken
        }
        mcp::McpServerAuthPolicy::OptionalOAuth { .. }
        | mcp::McpServerAuthPolicy::RequiredOAuth { .. } => {
            grant.provider_kind == credentials::CredentialProviderKind::McpOAuth
        }
    };
    if !kind_compatible {
        return Err(AgentApiError::rejected(format!(
            "credential grant {} provider kind {:?} is not compatible with MCP server {} auth policy",
            grant.grant_id, grant.provider_kind, record.server_id
        )));
    }

    if let Some(audience) = &grant.audience {
        if !credentials::audience_covers(audience, &record.server_url) {
            // This text can reach the model and session context through the notice on
            // unavailable servers, so it names the grant and not the URL.
            return Err(AgentApiError::rejected(format!(
                "credential grant {} audience does not cover this MCP server's URL",
                grant.grant_id
            )));
        }
        let oauth_resource = match &record.auth_policy {
            mcp::McpServerAuthPolicy::OptionalOAuth { resource, .. }
            | mcp::McpServerAuthPolicy::RequiredOAuth { resource, .. } => Some(resource),
            _ => None,
        };
        if let Some(resource) = oauth_resource
            && !credentials::audience_covers(audience, resource)
        {
            return Err(AgentApiError::rejected(format!(
                "credential grant {} audience does not cover this MCP server's OAuth resource",
                grant.grant_id
            )));
        }
    }
    Ok(())
}

pub(super) fn api_credential_provider_kind(
    value: credentials::CredentialProviderKind,
) -> api::CredentialProviderKind {
    match value {
        credentials::CredentialProviderKind::StaticToken => {
            api::CredentialProviderKind::StaticToken
        }
        credentials::CredentialProviderKind::McpOAuth => api::CredentialProviderKind::McpOAuth,
        credentials::CredentialProviderKind::GitHubApp => api::CredentialProviderKind::GitHubApp,
        credentials::CredentialProviderKind::CustomOAuth => {
            api::CredentialProviderKind::CustomOAuth
        }
        credentials::CredentialProviderKind::ModelApiKey => {
            api::CredentialProviderKind::ModelApiKey
        }
        credentials::CredentialProviderKind::ModelOAuth => api::CredentialProviderKind::ModelOAuth,
        credentials::CredentialProviderKind::ModelEndpoint => {
            api::CredentialProviderKind::ModelEndpoint
        }
    }
}

fn api_credential_grant_exposure(
    value: credentials::CredentialGrantExposure,
) -> api::CredentialGrantExposure {
    match value {
        credentials::CredentialGrantExposure::Brokered => api::CredentialGrantExposure::Brokered,
        credentials::CredentialGrantExposure::Retrievable => {
            api::CredentialGrantExposure::Retrievable
        }
    }
}

pub(super) fn registry_credential_grant_exposure(
    value: api::CredentialGrantExposure,
) -> credentials::CredentialGrantExposure {
    match value {
        api::CredentialGrantExposure::Brokered => credentials::CredentialGrantExposure::Brokered,
        api::CredentialGrantExposure::Retrievable => {
            credentials::CredentialGrantExposure::Retrievable
        }
    }
}

fn api_credential_grant_status(
    value: credentials::CredentialGrantStatus,
) -> api::CredentialGrantStatus {
    match value {
        credentials::CredentialGrantStatus::Active => api::CredentialGrantStatus::Active,
        credentials::CredentialGrantStatus::NeedsReauth => api::CredentialGrantStatus::NeedsReauth,
        credentials::CredentialGrantStatus::Revoked => api::CredentialGrantStatus::Revoked,
        credentials::CredentialGrantStatus::Failed => api::CredentialGrantStatus::Failed,
    }
}

pub(super) fn registry_credential_grant_status_for_filter(
    value: api::CredentialGrantStatus,
) -> credentials::CredentialGrantStatus {
    match value {
        api::CredentialGrantStatus::Active => credentials::CredentialGrantStatus::Active,
        api::CredentialGrantStatus::NeedsReauth => credentials::CredentialGrantStatus::NeedsReauth,
        api::CredentialGrantStatus::Revoked => credentials::CredentialGrantStatus::Revoked,
        api::CredentialGrantStatus::Failed => credentials::CredentialGrantStatus::Failed,
    }
}
