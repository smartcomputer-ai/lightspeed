use super::*;

pub(super) const MCP_SERVER_SECRET_NAMESPACE: &str = "mcp_server";

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
        approval_default: registry_approval(server.approval_default),
        defer_loading_default: server.defer_loading_default,
        allow_private_network: server.allow_private_network,
        auth_policy: registry_auth_policy(server.auth_policy),
        auth_grant_id,
        status: registry_status(server.status),
        now_ms,
    })
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
        approval_default: api_approval(record.approval_default),
        defer_loading_default: record.defer_loading_default,
        allow_private_network: record.allow_private_network,
        auth_policy: api_auth_policy(record.auth_policy),
        credential: record
            .auth_grant_id
            .map(|grant_id| api::McpServerCredential::AuthGrant {
                grant_id: grant_id.as_str().to_owned(),
            }),
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

/// Resolve one declared config link against its catalog record and grant
/// into the remote MCP tool spec. Shared by put-time validation and toolset
/// reconciliation, so a config put fails fast when a link cannot resolve.
pub(super) fn mcp_tool_from_config_link(
    _link: &engine::McpServerLink,
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

    let tool_name = default_mcp_tool_name(&record.default_server_label)?;
    let auth_ref = auth_ref_for_server(record, grant)?;
    Ok(engine::ToolSpec {
        name: tool_name,
        execution: engine::ToolExecutionSpec::default(),
        kind: engine::ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
            server_id: record.server_id.as_str().to_owned(),
            record_revision: record.revision,
            server_label: record.default_server_label.clone(),
            server_url: record.server_url.clone(),
            description_ref: None,
            allowed_tools: record.allowed_tools.clone(),
            execution: engine_execution(record.execution),
            exposure: engine_exposure(record.exposure),
            approval: engine_approval(api_approval(record.approval_default)),
            defer_loading: record.defer_loading_default,
            auth_ref,
            auth_required: matches!(
                record.auth_policy,
                mcp::McpServerAuthPolicy::RequiredBearer
                    | mcp::McpServerAuthPolicy::RequiredOAuth { .. }
            ),
            allow_private_network: record.allow_private_network,
        }),
        parallelism: engine::ToolParallelism::ParallelSafe,
    })
}

impl super::session_preparation::SessionPreparationService {
    /// Resolve the config's declared MCP links into the desired remote tool
    /// specs, loading catalog records and auth grants. Used both to validate
    /// a config document at admission and to reconcile the session toolset.
    pub(crate) async fn desired_mcp_tools(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<BTreeMap<ToolName, engine::ToolSpec>, AgentApiError> {
        let Some(mcp) = features.mcp.as_ref() else {
            return Ok(BTreeMap::new());
        };
        let mut tools = BTreeMap::new();
        let mut search_descriptions = Vec::new();
        for link in &mcp.servers {
            let server_id = parse_mcp_server_id(link.server_id.clone())?;
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
            let tool = mcp_tool_from_config_link(link, &record, grant.as_ref())?;
            if record.execution == mcp::McpExecution::Native
                && record.exposure == mcp::McpExposure::Search
            {
                search_descriptions.push(format!(
                    "{}: {}",
                    record.server_id,
                    record.description.as_deref().unwrap_or("MCP tools")
                ));
            }
            if tools.insert(tool.name.clone(), tool).is_some() {
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
                "mcp.find_tools",
                settings.clone(),
                engine::ToolParallelism::ParallelSafe,
                engine::ToolExecutionSpec::new(engine::ToolExecutionClass::RemoteInteractive, true),
            );
            let call = tools::definitions::register(
                "mcp.call",
                settings,
                engine::ToolParallelism::Exclusive,
                engine::ToolExecutionSpec::new(
                    engine::ToolExecutionClass::RemoteInteractive,
                    false,
                ),
            );
            for tool in [find, call] {
                if tools.insert(tool.name.clone(), tool).is_some() {
                    return Err(AgentApiError::invalid_request(
                        "an MCP server id collides with a reserved native MCP search tool name",
                    ));
                }
            }
        }
        Ok(tools)
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
        api::RemoteMcpExecution::Native => mcp::McpExecution::Native,
    }
}

fn api_execution(value: mcp::McpExecution) -> api::RemoteMcpExecution {
    match value {
        mcp::McpExecution::Provider => api::RemoteMcpExecution::Provider,
        mcp::McpExecution::Native => api::RemoteMcpExecution::Native,
    }
}

fn engine_execution(value: mcp::McpExecution) -> engine::RemoteMcpExecution {
    match value {
        mcp::McpExecution::Provider => engine::RemoteMcpExecution::Provider,
        mcp::McpExecution::Native => engine::RemoteMcpExecution::Native,
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

fn engine_exposure(value: mcp::McpExposure) -> engine::RemoteMcpExposure {
    match value {
        mcp::McpExposure::Inject => engine::RemoteMcpExposure::Inject,
        mcp::McpExposure::Search => engine::RemoteMcpExposure::Search,
    }
}

fn api_approval(value: mcp::McpApprovalPolicy) -> api::RemoteMcpApprovalPolicy {
    match value {
        mcp::McpApprovalPolicy::Always => api::RemoteMcpApprovalPolicy::Always,
        mcp::McpApprovalPolicy::Never => api::RemoteMcpApprovalPolicy::Never,
    }
}

fn engine_approval(value: api::RemoteMcpApprovalPolicy) -> engine::RemoteMcpApprovalPolicy {
    match value {
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

fn default_mcp_tool_name(server_label: &str) -> Result<ToolName, AgentApiError> {
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
    parse_mcp_tool_name(tool_id)
}

fn parse_mcp_tool_name(tool_id: String) -> Result<ToolName, AgentApiError> {
    ToolName::try_new(tool_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid MCP tool id: {error}")))
}

fn auth_ref_for_server(
    record: &mcp::McpServerRecord,
    grant: Option<&auth::AuthGrantRecord>,
) -> Result<Option<engine::SecretRef>, AgentApiError> {
    match (&record.auth_policy, grant) {
        (mcp::McpServerAuthPolicy::None, Some(_)) => Err(AgentApiError::invalid_request(
            "an MCP server credential is only valid with an auth policy",
        )),
        (mcp::McpServerAuthPolicy::RequiredBearer, None)
        | (mcp::McpServerAuthPolicy::RequiredOAuth { .. }, None) => Err(AgentApiError::rejected(
            format!("MCP server requires an auth grant: {}", record.server_id),
        )),
        (_, Some(grant)) => {
            auth_api::validate_mcp_grant_for_server(record, grant)?;
            Ok(Some(engine::SecretRef {
                namespace: MCP_SERVER_SECRET_NAMESPACE.to_owned(),
                id: record.server_id.as_str().to_owned(),
            }))
        }
        (mcp::McpServerAuthPolicy::OptionalBearer, None)
        | (mcp::McpServerAuthPolicy::OptionalOAuth { .. }, None) => Ok(Some(engine::SecretRef {
            namespace: MCP_SERVER_SECRET_NAMESPACE.to_owned(),
            id: record.server_id.as_str().to_owned(),
        })),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_meta_tools_are_not_strict_functions() {
        let target = tools::runtime::ToolTarget::api_kind(engine::ProviderApiKind::OpenAiResponses);
        for id in ["mcp.call", "mcp.find_tools"] {
            let resolved = tools::definitions::resolve(
                &ToolName::new(id),
                &engine::BuiltinToolSpec::default(),
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
            annotations: None,
        }]);
        let api::McpServerToolsDiscoverResponse::Success { tools } = response else {
            panic!("discovery projection must succeed");
        };
        assert_eq!(tools[0].title.as_deref(), Some(retained.as_str()));
        assert_eq!(tools[0].description.as_deref(), Some(retained.as_str()));
    }
}

impl GatewayAgentApi {
    pub(super) fn preparation_service(
        &self,
    ) -> super::session_preparation::SessionPreparationService {
        super::session_preparation::SessionPreparationService {
            store: self.store.clone(),
            task_queue: self.task_queue.clone(),
        }
    }
    pub(super) async fn desired_mcp_tools(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<BTreeMap<ToolName, engine::ToolSpec>, AgentApiError> {
        self.preparation_service().desired_mcp_tools(features).await
    }
}
