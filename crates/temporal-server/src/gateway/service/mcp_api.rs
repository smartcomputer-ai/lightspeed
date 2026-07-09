use super::*;

pub(super) const AUTH_GRANT_SECRET_NAMESPACE: &str = "auth_grant";

pub(super) fn put_mcp_server_record(
    server: McpServerInput,
    now_ms: i64,
) -> Result<mcp::PutMcpServerRecord, AgentApiError> {
    Ok(mcp::PutMcpServerRecord {
        server_id: parse_mcp_server_id(server.server_id)?,
        display_name: server.display_name,
        server_url: server.server_url,
        transport: registry_transport(server.transport),
        default_server_label: server.default_server_label,
        description: server.description,
        allowed_tools: server.allowed_tools,
        approval_default: registry_approval(server.approval_default),
        defer_loading_default: server.defer_loading_default,
        auth_policy: registry_auth_policy(server.auth_policy),
        status: registry_status(server.status),
        now_ms,
    })
}

pub(super) fn mcp_server_view(record: mcp::McpServerRecord) -> api::McpServerView {
    api::McpServerView {
        server_id: record.server_id.as_str().to_owned(),
        display_name: record.display_name,
        server_url: record.server_url,
        transport: api_transport(record.transport),
        default_server_label: record.default_server_label,
        description: record.description,
        allowed_tools: record.allowed_tools,
        approval_default: api_approval(record.approval_default),
        defer_loading_default: record.defer_loading_default,
        auth_policy: api_auth_policy(record.auth_policy),
        status: api_status(record.status),
        revision: record.revision,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

/// Resolve one declared config link against its catalog record and grant
/// into the remote MCP tool spec. Shared by put-time validation and toolset
/// reconciliation, so a config put fails fast when a link cannot resolve.
pub(super) fn mcp_tool_from_config_link(
    link: &engine::McpServerLink,
    record: &mcp::McpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<engine::ToolSpec, AgentApiError> {
    match record.status {
        mcp::McpServerStatus::Disabled => {
            return Err(AgentApiError::rejected(format!(
                "MCP server is disabled: {}",
                record.server_id
            )));
        }
        mcp::McpServerStatus::NeedsAuthConfig => {
            return Err(AgentApiError::rejected(format!(
                "MCP server needs auth configuration before linking: {}",
                record.server_id
            )));
        }
        mcp::McpServerStatus::Active | mcp::McpServerStatus::Unverified => {}
    }

    let tool_name = default_mcp_tool_name(&record.server_id)?;
    let auth_ref = auth_ref_for_link(record, grant)?;
    Ok(engine::ToolSpec {
        name: tool_name,
        kind: engine::ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
            server_label: record.default_server_label.clone(),
            server_url: record.server_url.clone(),
            description_ref: None,
            allowed_tools: link
                .allowed_tools
                .clone()
                .or_else(|| record.allowed_tools.clone()),
            approval: link
                .approval
                .clone()
                .unwrap_or_else(|| engine_approval(api_approval(record.approval_default))),
            defer_loading: link.defer_loading.or(record.defer_loading_default),
            auth_ref,
        }),
        parallelism: engine::ToolParallelism::ParallelSafe,
        target_requirement: engine::ToolTargetRequirement::None,
    })
}

impl GatewayAgentApi {
    /// Resolve the config's declared MCP links into the desired remote tool
    /// specs, loading catalog records and auth grants. Used both to validate
    /// a config document at admission and to reconcile the session toolset.
    pub(super) async fn desired_mcp_tools(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<BTreeMap<ToolName, engine::ToolSpec>, AgentApiError> {
        let Some(mcp) = features.mcp.as_ref() else {
            return Ok(BTreeMap::new());
        };
        let mut tools = BTreeMap::new();
        for link in &mcp.servers {
            let server_id = parse_mcp_server_id(link.server_id.clone())?;
            let record = self
                .store
                .read_server(&server_id)
                .await
                .map_err(map_mcp_error)?;
            let grant = match link.auth_grant_id.clone() {
                Some(grant_id) => {
                    let grant_id = parse_auth_grant_id(grant_id)?;
                    Some(
                        self.store
                            .read_grant(&grant_id)
                            .await
                            .map_err(map_auth_error)?,
                    )
                }
                None => None,
            };
            let tool = mcp_tool_from_config_link(link, &record, grant.as_ref())?;
            tools.insert(tool.name.clone(), tool);
        }
        Ok(tools)
    }
}

fn registry_transport(value: api::RemoteMcpTransport) -> mcp::RemoteMcpTransport {
    match value {
        api::RemoteMcpTransport::StreamableHttp => mcp::RemoteMcpTransport::StreamableHttp,
        api::RemoteMcpTransport::Sse => mcp::RemoteMcpTransport::Sse,
        api::RemoteMcpTransport::Auto => mcp::RemoteMcpTransport::Auto,
    }
}

fn api_transport(value: mcp::RemoteMcpTransport) -> api::RemoteMcpTransport {
    match value {
        mcp::RemoteMcpTransport::StreamableHttp => api::RemoteMcpTransport::StreamableHttp,
        mcp::RemoteMcpTransport::Sse => api::RemoteMcpTransport::Sse,
        mcp::RemoteMcpTransport::Auto => api::RemoteMcpTransport::Auto,
    }
}

fn registry_approval(value: api::RemoteMcpApprovalPolicy) -> mcp::McpApprovalPolicy {
    match value {
        api::RemoteMcpApprovalPolicy::ProviderDefault => mcp::McpApprovalPolicy::ProviderDefault,
        api::RemoteMcpApprovalPolicy::Always => mcp::McpApprovalPolicy::Always,
        api::RemoteMcpApprovalPolicy::Never => mcp::McpApprovalPolicy::Never,
    }
}

fn api_approval(value: mcp::McpApprovalPolicy) -> api::RemoteMcpApprovalPolicy {
    match value {
        mcp::McpApprovalPolicy::ProviderDefault => api::RemoteMcpApprovalPolicy::ProviderDefault,
        mcp::McpApprovalPolicy::Always => api::RemoteMcpApprovalPolicy::Always,
        mcp::McpApprovalPolicy::Never => api::RemoteMcpApprovalPolicy::Never,
    }
}

fn engine_approval(value: api::RemoteMcpApprovalPolicy) -> engine::RemoteMcpApprovalPolicy {
    match value {
        api::RemoteMcpApprovalPolicy::ProviderDefault => {
            engine::RemoteMcpApprovalPolicy::ProviderDefault
        }
        api::RemoteMcpApprovalPolicy::Always => engine::RemoteMcpApprovalPolicy::Always,
        api::RemoteMcpApprovalPolicy::Never => engine::RemoteMcpApprovalPolicy::Never,
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

fn default_mcp_tool_name(server_id: &mcp::McpServerId) -> Result<ToolName, AgentApiError> {
    let mut tool_id = String::from("mcp_");
    for ch in server_id.as_str().chars() {
        if tool_id.len() >= 64 {
            break;
        }
        tool_id.push(if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        });
    }
    parse_mcp_tool_name(tool_id)
}

fn parse_mcp_tool_name(tool_id: String) -> Result<ToolName, AgentApiError> {
    ToolName::try_new(tool_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid MCP tool id: {error}")))
}

fn auth_ref_for_link(
    record: &mcp::McpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<Option<engine::SecretRef>, AgentApiError> {
    match (&record.auth_policy, grant) {
        (mcp::McpServerAuthPolicy::None, Some(_)) => Err(AgentApiError::invalid_request(
            "authGrantId is only valid for MCP servers with an auth policy",
        )),
        (mcp::McpServerAuthPolicy::RequiredBearer, None)
        | (mcp::McpServerAuthPolicy::RequiredOAuth { .. }, None) => Err(AgentApiError::rejected(
            format!("MCP server requires an auth grant: {}", record.server_id),
        )),
        (_, Some(grant)) => {
            auth_api::validate_mcp_grant_for_link(record, grant)?;
            Ok(Some(engine::SecretRef {
                namespace: AUTH_GRANT_SECRET_NAMESPACE.to_owned(),
                id: grant.grant_id.as_str().to_owned(),
            }))
        }
        (_, None) => Ok(None),
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
