use super::*;

pub(super) fn put_mcp_server_record(
    server: McpServerInput,
    now_ms: i64,
) -> Result<mcp::PutMcpServerRecord, AgentApiError> {
    let auth_grant_id = match server.credential {
        Some(api::McpServerCredential::AuthGrant { grant_id }) => {
            Some(parse_auth_grant_id(grant_id)?)
        }
        None => None,
    };
    let headers = registry_headers(server.headers)?;
    Ok(mcp::PutMcpServerRecord {
        server_id: parse_mcp_server_id(server.server_id)?,
        display_name: server.display_name,
        server_url: server.server_url,
        transport: mcp::RemoteMcpTransport::default(),
        default_server_label: server.default_server_label,
        description: server.description,
        allowed_tools: server.allowed_tools,
        execution: registry_execution(server.execution),
        exposure: registry_exposure(server.exposure),
        approval: registry_approval(server.approval),
        defer_loading: server.defer_loading,
        allow_private_network: server.allow_private_network,
        auth_policy: registry_auth_policy(server.auth_policy),
        auth_grant_id,
        headers,
        status: registry_status(server.status),
        now_ms,
    })
}

/// Lowercase each configured header name. Two input names that differ only
/// by case are the same header and are refused rather than merged.
fn registry_headers(
    headers: BTreeMap<String, api::McpServerHeader>,
) -> Result<mcp::McpServerHeaders, AgentApiError> {
    let mut registry = mcp::McpServerHeaders::new();
    for (name, value) in headers {
        let normalized = mcp::normalize_mcp_server_header_name(&name).map_err(map_mcp_error)?;
        let value = match value {
            api::McpServerHeader::Literal { value } => mcp::McpServerHeaderValue::Literal { value },
            api::McpServerHeader::Credential { grant_id } => {
                mcp::McpServerHeaderValue::Credential {
                    grant_id: parse_auth_grant_id(grant_id)?,
                }
            }
        };
        if registry.insert(normalized.clone(), value).is_some() {
            return Err(AgentApiError::invalid_request(format!(
                "duplicate MCP header {normalized:?}; header names are case-insensitive"
            )));
        }
    }
    Ok(registry)
}

fn api_headers(headers: mcp::McpServerHeaders) -> BTreeMap<String, api::McpServerHeader> {
    headers
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                mcp::McpServerHeaderValue::Literal { value } => {
                    api::McpServerHeader::Literal { value }
                }
                mcp::McpServerHeaderValue::Credential { grant_id } => {
                    api::McpServerHeader::Credential {
                        grant_id: grant_id.as_str().to_owned(),
                    }
                }
            };
            (name, value)
        })
        .collect()
}

/// A header credential must be an active static token whose audience, when
/// set, covers the server URL. OAuth grants are refused: their access tokens
/// would be sent raw to an arbitrary endpoint, and refresh stays out of header
/// resolution.
pub(super) fn validate_mcp_header_grant(
    record: &mcp::PutMcpServerRecord,
    header: &str,
    grant: &auth::AuthGrantRecord,
) -> Result<(), AgentApiError> {
    if grant.status != auth::AuthGrantStatus::Active {
        return Err(AgentApiError::rejected(format!(
            "credential {} for MCP header {header:?} is not active: {:?}",
            grant.grant_id, grant.status
        )));
    }
    if grant.provider_kind != auth::AuthProviderKind::StaticBearer {
        return Err(AgentApiError::rejected(format!(
            "credential {} for MCP header {header:?} must be a static token, not {:?}",
            grant.grant_id, grant.provider_kind
        )));
    }
    if let Some(audience) = &grant.audience
        && !auth::audience_covers(audience, &record.server_url)
    {
        return Err(AgentApiError::rejected(format!(
            "credential {} for MCP header {header:?} does not cover this MCP server's URL",
            grant.grant_id
        )));
    }
    Ok(())
}

pub(super) fn mcp_server_view(record: mcp::McpServerRecord) -> api::McpServerView {
    api::McpServerView {
        server_id: record.server_id.as_str().to_owned(),
        display_name: record.display_name,
        server_url: record.server_url,
        default_server_label: record.default_server_label,
        description: record.description,
        allowed_tools: record.allowed_tools,
        execution: api_execution(record.execution),
        exposure: api_exposure(record.exposure),
        approval: api_approval(record.approval),
        defer_loading: record.defer_loading,
        allow_private_network: record.allow_private_network,
        auth_policy: api_auth_policy(record.auth_policy),
        credential: record
            .auth_grant_id
            .map(|grant_id| api::McpServerCredential::AuthGrant {
                grant_id: grant_id.as_str().to_owned(),
            }),
        headers: api_headers(record.headers),
        status: api_status(record.status),
        revision: record.revision,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn mcp_tool_discovery_success(
    tools: Vec<mcp::DiscoveredMcpTool>,
) -> api::McpServerToolsDiscoverResponse {
    api::McpServerToolsDiscoverResponse::Success {
        tools: tools
            .into_iter()
            .map(|tool| api::McpAdvertisedToolView {
                name: tool.name,
                title: tool.title,
                description: tool.description,
                annotations: tool
                    .annotations
                    .map(|annotations| api::McpToolAnnotationsView {
                        read_only_hint: annotations.read_only_hint,
                        destructive_hint: annotations.destructive_hint,
                        idempotent_hint: annotations.idempotent_hint,
                        open_world_hint: annotations.open_world_hint,
                    }),
            })
            .collect(),
    }
}

pub(super) fn mcp_tool_discovery_failure(
    failure: mcp::McpToolDiscoveryFailure,
) -> api::McpServerToolsDiscoverResponse {
    use mcp::McpToolDiscoveryFailureKind as Source;
    let code = match failure.kind {
        Source::CredentialAbsent => api::McpToolDiscoveryFailureCode::CredentialAbsent,
        Source::GrantNeedsReauth => api::McpToolDiscoveryFailureCode::GrantNeedsReauth,
        Source::GrantAudienceMismatch => api::McpToolDiscoveryFailureCode::GrantAudienceMismatch,
        Source::Unauthorized => api::McpToolDiscoveryFailureCode::Unauthorized,
        Source::Forbidden => api::McpToolDiscoveryFailureCode::Forbidden,
        Source::AdditionalConsentRequired => {
            api::McpToolDiscoveryFailureCode::AdditionalConsentRequired
        }
        Source::RemoteRateLimited => api::McpToolDiscoveryFailureCode::RemoteRateLimited,
        Source::RemoteFailure => api::McpToolDiscoveryFailureCode::RemoteFailure,
        Source::Unreachable => api::McpToolDiscoveryFailureCode::Unreachable,
        Source::InvalidResponse => api::McpToolDiscoveryFailureCode::InvalidResponse,
        Source::UnsupportedProtocol => api::McpToolDiscoveryFailureCode::UnsupportedProtocol,
        Source::PaginationLimit => api::McpToolDiscoveryFailureCode::PaginationLimit,
        Source::ResponseTooLarge => api::McpToolDiscoveryFailureCode::ResponseTooLarge,
    };
    api::McpServerToolsDiscoverResponse::Failure {
        code,
        message: failure.message,
        required_scopes: failure.required_scopes,
    }
}

pub(super) fn validate_mcp_server_credential(
    record: &mcp::PutMcpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<(), AgentApiError> {
    match (&record.auth_policy, grant) {
        (mcp::McpServerAuthPolicy::None, Some(_)) => Err(AgentApiError::invalid_request(
            "MCP servers with auth policy none cannot bind a credential",
        )),
        (mcp::McpServerAuthPolicy::RequiredBearer, None)
        | (mcp::McpServerAuthPolicy::RequiredOAuth { .. }, None)
            if record.status != mcp::McpServerStatus::NeedsAuthConfig =>
        {
            Err(AgentApiError::rejected(format!(
                "active MCP server requires an auth grant: {}",
                record.server_id
            )))
        }
        (_, Some(grant)) => {
            let preview = record.clone().into_record();
            auth_api::validate_mcp_grant_for_server(&preview, grant)
        }
        _ => Ok(()),
    }
}

/// Resolve one declared config attachment against its catalog record and grant
/// into the remote MCP tool spec. Shared by put-time validation and toolset
/// reconciliation, so what refuses a newly named attachment is what leaves an
/// already attached one out of a later run.
pub(super) fn mcp_tool_from_config_attachment(
    attachment: &harness::McpServerAttachment,
    record: &mcp::McpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<harness::ToolSpec, AgentApiError> {
    match record.status {
        mcp::McpServerStatus::Disabled => {
            return Err(AgentApiError::rejected(format!(
                "MCP server is disabled: {}",
                record.server_id
            )));
        }
        mcp::McpServerStatus::NeedsAuthConfig => {
            return Err(AgentApiError::rejected(format!(
                "MCP server needs auth configuration before attaching: {}",
                record.server_id
            )));
        }
        mcp::McpServerStatus::Active | mcp::McpServerStatus::Unverified => {}
    }

    let allowed_tools = match &attachment.tools {
        Some(tools) => {
            if let Some(allowed) = &record.allowed_tools
                && let Some(tool) = tools.iter().find(|tool| !allowed.contains(tool))
            {
                return Err(AgentApiError::invalid_request(format!(
                    "MCP tool {tool} is outside the allowlist for server {}",
                    record.server_id
                )));
            }
            Some(tools.clone())
        }
        None => record.allowed_tools.clone(),
    };
    let tool_id = default_mcp_tool_id(&record.default_server_label)?;
    validate_server_auth(record, grant)?;
    Ok(harness::ToolSpec {
        id: tool_id,
        execution: harness::ToolExecutionSpec::new(
            harness::ToolExecutionClass::RemoteInteractive,
            false,
        ),
        kind: harness::ToolKind::RemoteMcp(harness::RemoteMcpToolSpec {
            server_id: record.server_id.as_str().to_owned(),
            record_revision: record.revision,
            server_label: record.default_server_label.clone(),
            description_ref: None,
            allowed_tools,
            execution: harness_execution(record.execution),
            exposure: harness_exposure(record.exposure),
            approval: harness_approval(api_approval(record.approval)),
            defer_loading: record.defer_loading,
        }),
        parallelism: harness::ToolParallelism::ParallelSafe,
    })
}

/// What a configuration's MCP attachments resolve to right now.
pub(crate) struct McpInventory {
    pub(crate) tools: BTreeMap<ToolId, harness::ToolSpec>,
    /// Attachments that resolved to nothing, in document order.
    pub(crate) unavailable: Vec<UnavailableMcpServer>,
}

pub(crate) struct UnavailableMcpServer {
    pub(crate) attachment: harness::McpServerAttachment,
    pub(crate) error: AgentApiError,
}

/// Whether a refused resolution describes the attachment itself: a server that is
/// disabled, deleted or awaiting auth, a grant no longer usable, a narrowed
/// allowlist. An outage says nothing about the server and fails the caller instead,
/// so it can retry rather than lose the tool.
pub(super) fn mcp_attachment_is_unavailable(error: &AgentApiError) -> bool {
    matches!(
        error.kind,
        AgentApiErrorKind::NotFound
            | AgentApiErrorKind::Rejected
            | AgentApiErrorKind::InvalidRequest
    )
}

/// The notice telling the model which attached servers it cannot use, or nothing
/// when it can use them all. It sits in context, so an operator reading the session
/// sees why a tool is missing as well.
pub(super) async fn mcp_catalog_entry(
    blobs: &dyn BlobStore,
    unavailable: &[UnavailableMcpServer],
) -> Result<Option<harness::ContextEntryInput>, AgentApiError> {
    if unavailable.is_empty() {
        return Ok(None);
    }
    let body = mcp_catalog_text(unavailable);
    let content_ref = blobs
        .put_bytes(body.into_bytes())
        .await
        .map_err(map_blob_store_error)?;
    let title = "Unavailable MCP servers";
    Ok(Some(harness::ContextEntryInput {
        kind: harness::ContextEntryKind::Catalog {
            title: title.to_owned(),
        },
        content: harness::ContentRef {
            content_ref,
            media_type: Some("text/markdown".to_owned()),
            provider_kind: None,
        },
        preview: Some(title.to_owned()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }))
}

fn mcp_catalog_text(unavailable: &[UnavailableMcpServer]) -> String {
    let mut text = String::from(
        "These MCP servers are attached to this session but unavailable right now, so their \
         tools are not offered. If a task needs one of them, say so rather than working around \
         its absence.\n\n",
    );
    for server in unavailable {
        text.push_str(&format!(
            "- {}: {}\n",
            server.attachment.server_id, server.error.message
        ));
    }
    text
}

impl super::session_preparation::SessionPreparationService {
    async fn resolve_mcp_attachment(
        &self,
        attachment: &harness::McpServerAttachment,
    ) -> Result<(harness::ToolSpec, mcp::McpServerRecord), AgentApiError> {
        let server_id = parse_mcp_server_id(attachment.server_id.clone())?;
        let record = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_error)?;
        let grant = match record.auth_grant_id.as_ref() {
            Some(grant_id) => Some(
                self.store
                    .read_grant(grant_id)
                    .await
                    .map_err(map_auth_error)?,
            ),
            None => None,
        };
        let tool = mcp_tool_from_config_attachment(attachment, &record, grant.as_ref())?;
        Ok((tool, record))
    }

    /// Resolve the config's declared MCP attachments against the catalog as it is
    /// now. An attachment that cannot resolve is reported rather than refused:
    /// whether that stops anything depends on who named it, and only a caller naming
    /// it for the first time should hear no.
    pub(crate) async fn mcp_inventory(
        &self,
        features: &harness::FeaturesConfig,
    ) -> Result<McpInventory, AgentApiError> {
        let mut tools = BTreeMap::new();
        let mut unavailable = Vec::new();
        let Some(mcp) = features.mcp.as_ref() else {
            return Ok(McpInventory { tools, unavailable });
        };
        let mut search_descriptions = Vec::new();
        for attachment in &mcp.servers {
            let (tool, record) = match self.resolve_mcp_attachment(attachment).await {
                Ok(resolved) => resolved,
                Err(error) if mcp_attachment_is_unavailable(&error) => {
                    unavailable.push(UnavailableMcpServer {
                        attachment: attachment.clone(),
                        error,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            if record.execution == mcp::McpExecution::Runtime
                && record.exposure == mcp::McpExposure::Search
            {
                search_descriptions.push(format!(
                    "{}: {}",
                    record.server_id,
                    record.description.as_deref().unwrap_or("MCP tools")
                ));
            }
            if tools.insert(tool.id.clone(), tool).is_some() {
                return Err(AgentApiError::invalid_request(format!(
                    "MCP server tool names collide after normalization: {}",
                    record.server_id
                )));
            }
        }
        if !search_descriptions.is_empty() {
            search_descriptions.sort();
            let index = search_descriptions.join("; ");
            let settings = tools::definitions::BuiltinSettings {
                mcp_server_index: index,
                ..Default::default()
            };
            let find = tools::definitions::register(
                harness::MCP_FIND_TOOLS_TOOL_ID,
                settings.clone(),
                harness::ToolParallelism::ParallelSafe,
                harness::ToolExecutionSpec::new(
                    harness::ToolExecutionClass::RemoteInteractive,
                    true,
                ),
            );
            let call = tools::definitions::register(
                harness::MCP_CALL_TOOL_ID,
                settings,
                harness::ToolParallelism::Exclusive,
                harness::ToolExecutionSpec::new(
                    harness::ToolExecutionClass::RemoteInteractive,
                    false,
                ),
            );
            for tool in [find, call] {
                if tools.insert(tool.id.clone(), tool).is_some() {
                    return Err(AgentApiError::invalid_request(
                        "an MCP server id collides with a reserved runtime MCP search tool name",
                    ));
                }
            }
        }
        Ok(McpInventory { tools, unavailable })
    }
}

fn registry_approval(value: api::RemoteMcpApprovalPolicy) -> mcp::McpApprovalPolicy {
    match value {
        api::RemoteMcpApprovalPolicy::Always => mcp::McpApprovalPolicy::Always,
        api::RemoteMcpApprovalPolicy::Never => mcp::McpApprovalPolicy::Never,
    }
}

fn registry_execution(value: api::RemoteMcpExecution) -> mcp::McpExecution {
    match value {
        api::RemoteMcpExecution::Provider => mcp::McpExecution::Provider,
        api::RemoteMcpExecution::Runtime => mcp::McpExecution::Runtime,
    }
}

fn api_execution(value: mcp::McpExecution) -> api::RemoteMcpExecution {
    match value {
        mcp::McpExecution::Provider => api::RemoteMcpExecution::Provider,
        mcp::McpExecution::Runtime => api::RemoteMcpExecution::Runtime,
    }
}

fn harness_execution(value: mcp::McpExecution) -> harness::RemoteMcpExecution {
    match value {
        mcp::McpExecution::Provider => harness::RemoteMcpExecution::Provider,
        mcp::McpExecution::Runtime => harness::RemoteMcpExecution::Runtime,
    }
}

fn registry_exposure(value: api::RemoteMcpExposure) -> mcp::McpExposure {
    match value {
        api::RemoteMcpExposure::Inject => mcp::McpExposure::Inject,
        api::RemoteMcpExposure::Search => mcp::McpExposure::Search,
    }
}

fn api_exposure(value: mcp::McpExposure) -> api::RemoteMcpExposure {
    match value {
        mcp::McpExposure::Inject => api::RemoteMcpExposure::Inject,
        mcp::McpExposure::Search => api::RemoteMcpExposure::Search,
    }
}

fn harness_exposure(value: mcp::McpExposure) -> harness::RemoteMcpExposure {
    match value {
        mcp::McpExposure::Inject => harness::RemoteMcpExposure::Inject,
        mcp::McpExposure::Search => harness::RemoteMcpExposure::Search,
    }
}

fn api_approval(value: mcp::McpApprovalPolicy) -> api::RemoteMcpApprovalPolicy {
    match value {
        mcp::McpApprovalPolicy::Always => api::RemoteMcpApprovalPolicy::Always,
        mcp::McpApprovalPolicy::Never => api::RemoteMcpApprovalPolicy::Never,
    }
}

fn harness_approval(value: api::RemoteMcpApprovalPolicy) -> harness::RemoteMcpApprovalPolicy {
    match value {
        api::RemoteMcpApprovalPolicy::Always => harness::RemoteMcpApprovalPolicy::Always,
        api::RemoteMcpApprovalPolicy::Never => harness::RemoteMcpApprovalPolicy::Never,
    }
}

fn registry_auth_policy(value: api::McpServerAuthPolicy) -> mcp::McpServerAuthPolicy {
    match value {
        api::McpServerAuthPolicy::None => mcp::McpServerAuthPolicy::None,
        api::McpServerAuthPolicy::OptionalBearer => mcp::McpServerAuthPolicy::OptionalBearer,
        api::McpServerAuthPolicy::RequiredBearer => mcp::McpServerAuthPolicy::RequiredBearer,
        api::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => mcp::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        },
        api::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => mcp::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        },
    }
}

fn api_auth_policy(value: mcp::McpServerAuthPolicy) -> api::McpServerAuthPolicy {
    match value {
        mcp::McpServerAuthPolicy::None => api::McpServerAuthPolicy::None,
        mcp::McpServerAuthPolicy::OptionalBearer => api::McpServerAuthPolicy::OptionalBearer,
        mcp::McpServerAuthPolicy::RequiredBearer => api::McpServerAuthPolicy::RequiredBearer,
        mcp::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => api::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        },
        mcp::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => api::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        },
    }
}

fn registry_status(value: api::McpServerStatus) -> mcp::McpServerStatus {
    match value {
        api::McpServerStatus::Active => mcp::McpServerStatus::Active,
        api::McpServerStatus::NeedsAuthConfig => mcp::McpServerStatus::NeedsAuthConfig,
        api::McpServerStatus::Unverified => mcp::McpServerStatus::Unverified,
        api::McpServerStatus::Disabled => mcp::McpServerStatus::Disabled,
    }
}

pub(super) fn parse_mcp_server_id(server_id: String) -> Result<mcp::McpServerId, AgentApiError> {
    mcp::McpServerId::try_new(server_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid MCP server id: {error}")))
}

pub(super) fn map_mcp_error(error: mcp::McpRegistryError) -> AgentApiError {
    match error {
        mcp::McpRegistryError::AlreadyExists { server_id } => {
            AgentApiError::conflict(format!("MCP server already exists: {server_id}"))
        }
        mcp::McpRegistryError::NotFound { server_id } => {
            AgentApiError::not_found(format!("MCP server not found: {server_id}"))
        }
        mcp::McpRegistryError::RevisionConflict {
            server_id,
            expected,
            actual,
        } => AgentApiError::conflict(format!(
            "MCP server revision conflict for {server_id}: expected {expected}, got {actual}"
        )),
        mcp::McpRegistryError::InvalidInput { message } => AgentApiError::invalid_request(message),
        mcp::McpRegistryError::Store { message } => AgentApiError::internal(message),
    }
}

fn default_mcp_tool_id(server_label: &str) -> Result<ToolId, AgentApiError> {
    let mut tool_id = String::from("mcp_");
    for ch in server_label.chars() {
        if tool_id.len() >= 64 {
            break;
        }
        tool_id.push(if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        });
    }
    parse_mcp_tool_id(tool_id)
}

fn parse_mcp_tool_id(tool_id: String) -> Result<ToolId, AgentApiError> {
    ToolId::try_new(tool_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid MCP tool id: {error}")))
}

fn validate_server_auth(
    record: &mcp::McpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<(), AgentApiError> {
    match (&record.auth_policy, grant) {
        (mcp::McpServerAuthPolicy::None, Some(_)) => Err(AgentApiError::invalid_request(
            "an MCP server credential is only valid with an auth policy",
        )),
        (mcp::McpServerAuthPolicy::RequiredBearer, None)
        | (mcp::McpServerAuthPolicy::RequiredOAuth { .. }, None) => Err(AgentApiError::rejected(
            format!("MCP server requires an auth grant: {}", record.server_id),
        )),
        (_, Some(grant)) => auth_api::validate_mcp_grant_for_server(record, grant),
        (_, None) => Ok(()),
    }
}

pub(super) fn registry_status_for_filter(value: api::McpServerStatus) -> mcp::McpServerStatus {
    registry_status(value)
}

fn api_status(value: mcp::McpServerStatus) -> api::McpServerStatus {
    match value {
        mcp::McpServerStatus::Active => api::McpServerStatus::Active,
        mcp::McpServerStatus::NeedsAuthConfig => api::McpServerStatus::NeedsAuthConfig,
        mcp::McpServerStatus::Unverified => api::McpServerStatus::Unverified,
        mcp::McpServerStatus::Disabled => api::McpServerStatus::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal becomes the notice in session context, so the URL must stay out.
    #[test]
    fn the_notice_on_a_server_its_grant_stopped_covering_holds_no_url() {
        let attachment = harness::McpServerAttachment {
            server_id: "crm".to_owned(),
            tools: None,
        };
        let record = |server_url: &str| {
            mcp::PutMcpServerRecord {
                server_id: mcp::McpServerId::new("crm"),
                display_name: None,
                server_url: server_url.to_owned(),
                transport: mcp::RemoteMcpTransport::default(),
                default_server_label: "crm".to_owned(),
                description: None,
                allowed_tools: None,
                execution: mcp::McpExecution::Runtime,
                exposure: mcp::McpExposure::Inject,
                approval: mcp::McpApprovalPolicy::Never,
                defer_loading: None,
                allow_private_network: false,
                auth_policy: mcp::McpServerAuthPolicy::RequiredOAuth {
                    resource: "https://resource.example.com/SECRET".to_owned(),
                    scopes_default: Vec::new(),
                    protected_resource_metadata_url: None,
                    authorization_server: None,
                },
                auth_grant_id: Some(auth::AuthGrantId::new("authgrant_1")),
                headers: Default::default(),
                status: mcp::McpServerStatus::Active,
                now_ms: 1,
            }
            .into_record()
        };
        let grant = auth::CreateAuthGrantRecord {
            grant_id: auth::AuthGrantId::new("authgrant_1"),
            provider_id: "oauth".to_owned(),
            provider_kind: auth::AuthProviderKind::McpOAuth,
            exposure: auth::AuthGrantExposure::Brokered,
            created_by: Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: Some("https://crm.example.com".to_owned()),
            access_token_secret: None,
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: auth::AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 1,
        }
        .into_record();

        for server_url in [
            // The admin moved the server, so the grant no longer covers its URL.
            "https://moved.example.com/s/SECRET/mcp?key=SECRET",
            // The URL is covered and the OAuth resource is not.
            "https://crm.example.com/s/SECRET/mcp",
        ] {
            let error =
                mcp_tool_from_config_attachment(&attachment, &record(server_url), Some(&grant))
                    .expect_err("the grant does not cover the server");
            assert!(mcp_attachment_is_unavailable(&error));
            let notice = mcp_catalog_text(&[UnavailableMcpServer {
                attachment: attachment.clone(),
                error,
            }]);
            assert!(notice.contains("authgrant_1"), "{notice}");
            assert!(!notice.contains("SECRET"), "{notice}");
        }
    }

    #[test]
    fn search_meta_tools_are_not_strict_functions() {
        let target =
            tools::runtime::ToolTarget::api_kind(harness::ProviderApiKind::OpenAiResponses);
        for id in ["mcp.call", "mcp.find_tools"] {
            let resolved = tools::definitions::resolve(
                &ToolId::new(id),
                &harness::BuiltinToolSpec::default(),
                &target,
            )
            .expect("resolve meta-tool");
            let tools::definitions::Definition::Function(function) = &resolved[0].definition else {
                panic!("MCP search meta-tool must be a function");
            };
            assert_eq!(function.strict, Some(false));
        }
    }

    #[test]
    fn discovery_projection_preserves_discoverer_retained_text() {
        let retained = "d".repeat(mcp::McpToolDiscoveryLimits::default().max_text_bytes);
        let response = mcp_tool_discovery_success(vec![mcp::DiscoveredMcpTool {
            name: "long_description".to_owned(),
            title: Some(retained.clone()),
            description: Some(retained.clone()),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            annotations: None,
        }]);
        let api::McpServerToolsDiscoverResponse::Success { tools } = response else {
            panic!("discovery projection must succeed");
        };
        assert_eq!(tools[0].title.as_deref(), Some(retained.as_str()));
        assert_eq!(tools[0].description.as_deref(), Some(retained.as_str()));
    }
}
