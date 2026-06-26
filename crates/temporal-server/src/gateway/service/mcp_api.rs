use std::collections::BTreeSet;

use super::*;

pub(super) const AUTH_GRANT_SECRET_NAMESPACE: &str = "auth_grant";

impl GatewayAgentApi {
    pub(super) async fn wait_for_session_mcp_links(
        &self,
        session_id: &SessionId,
        expected_tool_ids: BTreeSet<ToolName>,
        baseline_failures: usize,
    ) -> Result<(SessionView, Vec<api::SessionMcpLinkView>), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for session MCP links to update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }

            let loaded = self.load_session_state(session_id).await?;
            let actual_tool_ids = linked_session_mcp_tool_ids(&loaded.state.tooling.tools);
            if actual_tool_ids == expected_tool_ids {
                let session = self.project_session_by_id(session_id).await?;
                let links = linked_session_mcp(&loaded.state.tooling.tools);
                return Ok((session, links));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

pub(super) fn create_mcp_server_record(
    params: McpServerCreateParams,
    created_at_ms: i64,
) -> Result<mcp::CreateMcpServerRecord, AgentApiError> {
    Ok(mcp::CreateMcpServerRecord {
        server_id: parse_mcp_server_id(params.server_id)?,
        display_name: params.display_name,
        server_url: params.server_url,
        transport: registry_transport(params.transport),
        default_server_label: params.default_server_label,
        description: params.description,
        allowed_tools: params.allowed_tools,
        approval_default: registry_approval(params.approval_default),
        defer_loading_default: params.defer_loading_default,
        auth_policy: registry_auth_policy(params.auth_policy),
        status: registry_status(params.status),
        created_at_ms,
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
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn session_mcp_link_from_record(
    params: SessionMcpLinkParams,
    record: &mcp::McpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<SessionMcpLinkDraft, AgentApiError> {
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

    let tool_name = match params.tool_id {
        Some(tool_id) => parse_mcp_tool_name(tool_id)?,
        None => default_mcp_tool_name(&record.server_id)?,
    };
    let auth_ref = auth_ref_for_link(record, grant)?;
    Ok(SessionMcpLinkDraft {
        tool_name,
        spec: engine::RemoteMcpToolSpec {
            server_label: params
                .server_label
                .unwrap_or_else(|| record.default_server_label.clone()),
            server_url: record.server_url.clone(),
            description_ref: None,
            allowed_tools: params
                .allowed_tools
                .or_else(|| record.allowed_tools.clone()),
            approval: params
                .approval
                .map(engine_approval)
                .unwrap_or_else(|| engine_approval(api_approval(record.approval_default))),
            defer_loading: params.defer_loading.or(record.defer_loading_default),
            auth_ref,
        },
    })
}

#[derive(Debug)]
pub(super) struct SessionMcpLinkDraft {
    pub(super) tool_name: ToolName,
    pub(super) spec: engine::RemoteMcpToolSpec,
}

pub(super) fn apply_session_mcp_link(
    tools: &BTreeMap<ToolName, engine::ToolSpec>,
    draft: SessionMcpLinkDraft,
) -> Result<engine::ToolPatch, AgentApiError> {
    if let Some(existing) = tools.get(&draft.tool_name) {
        if !matches!(existing.kind, engine::ToolKind::RemoteMcp(_)) {
            return Err(AgentApiError::conflict(format!(
                "tool id already exists and is not a remote MCP link: {}",
                draft.tool_name
            )));
        }
    }

    let patch = engine::ToolPatch {
        upsert: vec![engine::ToolSpec {
            name: draft.tool_name.clone(),
            kind: engine::ToolKind::RemoteMcp(draft.spec),
            parallelism: engine::ToolParallelism::ParallelSafe,
            target_requirement: engine::ToolTargetRequirement::None,
        }],
        remove: Vec::new(),
    };
    validate_mcp_patch(tools, &patch)?;
    Ok(patch)
}

pub(super) fn remove_session_mcp_link(
    tools: &BTreeMap<ToolName, engine::ToolSpec>,
    tool_name: &ToolName,
) -> Result<engine::ToolPatch, AgentApiError> {
    let tool = tools.get(tool_name).ok_or_else(|| {
        AgentApiError::not_found(format!("session MCP link not found: {tool_name}"))
    })?;
    if !matches!(tool.kind, engine::ToolKind::RemoteMcp(_)) {
        return Err(AgentApiError::invalid_request(format!(
            "tool is not a remote MCP link: {tool_name}"
        )));
    }

    let patch = engine::ToolPatch {
        upsert: Vec::new(),
        remove: vec![tool_name.clone()],
    };
    validate_mcp_patch(tools, &patch)?;
    Ok(patch)
}

pub(super) fn linked_session_mcp(
    tools: &BTreeMap<ToolName, engine::ToolSpec>,
) -> Vec<api::SessionMcpLinkView> {
    tools
        .iter()
        .filter_map(|(tool_name, tool)| {
            let engine::ToolKind::RemoteMcp(spec) = &tool.kind else {
                return None;
            };
            Some(api::SessionMcpLinkView {
                tool_id: tool_name.as_str().to_owned(),
                server_label: spec.server_label.clone(),
                server_url: spec.server_url.clone(),
                allowed_tools: spec.allowed_tools.clone(),
                approval: api_engine_approval(&spec.approval),
                defer_loading: spec.defer_loading,
                auth_ref: spec.auth_ref.as_ref().map(|auth_ref| api::SecretRefView {
                    namespace: auth_ref.namespace.clone(),
                    id: auth_ref.id.clone(),
                }),
            })
        })
        .collect()
}

pub(super) fn linked_session_mcp_tool_ids(
    tools: &BTreeMap<ToolName, engine::ToolSpec>,
) -> BTreeSet<ToolName> {
    tools
        .iter()
        .filter_map(|(tool_name, tool)| match &tool.kind {
            engine::ToolKind::RemoteMcp(_) => Some(tool_name.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn parse_mcp_server_id(server_id: String) -> Result<mcp::McpServerId, AgentApiError> {
    mcp::McpServerId::try_new(server_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid MCP server id: {error}")))
}

pub(super) fn parse_mcp_tool_name(tool_id: String) -> Result<ToolName, AgentApiError> {
    ToolName::try_new(tool_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid MCP tool id: {error}")))
}

pub(super) fn map_mcp_error(error: mcp::McpRegistryError) -> AgentApiError {
    match error {
        mcp::McpRegistryError::AlreadyExists { server_id } => {
            AgentApiError::conflict(format!("MCP server already exists: {server_id}"))
        }
        mcp::McpRegistryError::NotFound { server_id } => {
            AgentApiError::not_found(format!("MCP server not found: {server_id}"))
        }
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

fn validate_mcp_patch(
    tools: &BTreeMap<ToolName, engine::ToolSpec>,
    patch: &engine::ToolPatch,
) -> Result<(), AgentApiError> {
    patch
        .apply_to(tools)
        .map(|_| ())
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))
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

fn api_engine_approval(value: &engine::RemoteMcpApprovalPolicy) -> api::RemoteMcpApprovalPolicy {
    match value {
        engine::RemoteMcpApprovalPolicy::ProviderDefault => {
            api::RemoteMcpApprovalPolicy::ProviderDefault
        }
        engine::RemoteMcpApprovalPolicy::Always => api::RemoteMcpApprovalPolicy::Always,
        engine::RemoteMcpApprovalPolicy::Never => api::RemoteMcpApprovalPolicy::Never,
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
