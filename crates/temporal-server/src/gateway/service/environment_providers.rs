use super::*;

use environment_registry::{
    EnvironmentProviderCapabilities as RegistryProviderCapabilities,
    EnvironmentProviderHeartbeat as RegistryProviderHeartbeat, EnvironmentProviderId,
    EnvironmentProviderKind as RegistryProviderKind, EnvironmentProviderRecord,
    EnvironmentProviderStatus as RegistryProviderStatus, EnvironmentRegistryError,
    EnvironmentTargetRecord, HostControllerConnectionSpec, ListEnvironmentProviders,
    ListEnvironmentTargets, RegisterEnvironmentProvider, UpdateEnvironmentProviderStatus,
    UpsertEnvironmentTargetRecord,
};
use host_protocol::{
    control::{
        handshake::ControllerInitializeParams,
        targets::{HostTargetStatus, HostTargetSummary, ListTargetsParams},
    },
    shared::{
        CURRENT_PROTOCOL_VERSION, HostCapabilities, HostPath, HostScope, HostTargetId,
        HostTransport, ImplementationInfo,
    },
};

impl GatewayAgentApi {
    pub(super) async fn register_environment_provider_record(
        &self,
        params: EnvironmentProviderRegisterParams,
    ) -> Result<EnvironmentProviderRegisterResponse, AgentApiError> {
        let observed_at_ms = now_ms()?;
        let controller_connection = registry_controller_connection(params.controller_connection)?;
        let (capabilities, implementation) = self
            .initialize_environment_provider_controller(&controller_connection)
            .await?;
        let provider = environment_registry::EnvironmentProviderStore::register_provider(
            self.store.as_ref(),
            RegisterEnvironmentProvider {
                provider_id: parse_environment_provider_id(params.provider_id)?,
                provider_kind: registry_provider_kind(params.provider_kind),
                display_name: params.display_name,
                controller_connection,
                capabilities,
                implementation,
                lease_ttl_ms: params.lease_ttl_ms,
                metadata: params.metadata,
                observed_at_ms,
            },
        )
        .await
        .map_err(map_environment_registry_error)?;

        Ok(EnvironmentProviderRegisterResponse {
            provider: environment_provider_view(&provider),
        })
    }

    pub(super) async fn heartbeat_environment_provider_record(
        &self,
        params: EnvironmentProviderHeartbeatParams,
    ) -> Result<EnvironmentProviderHeartbeatResponse, AgentApiError> {
        let observed_at_ms = now_ms()?;
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let mut observed_targets = params
            .observed_targets
            .into_iter()
            .map(registry_target_summary)
            .collect::<Result<Vec<_>, _>>()?;

        let provider = environment_registry::EnvironmentProviderStore::update_provider_heartbeat(
            self.store.as_ref(),
            RegistryProviderHeartbeat {
                provider_id: provider_id.clone(),
                observed_at_ms,
                lease_ttl_ms: params.lease_ttl_ms,
                observed_targets: observed_targets.clone(),
            },
        )
        .await
        .map_err(map_environment_registry_error)?;

        if observed_targets.is_empty() && provider.capabilities.list_targets {
            observed_targets = self
                .list_environment_provider_targets(&provider)
                .await?
                .targets;
        }

        let mut targets = Vec::with_capacity(observed_targets.len());
        for target in observed_targets {
            let target = environment_registry::EnvironmentTargetStore::upsert_target(
                self.store.as_ref(),
                UpsertEnvironmentTargetRecord::from((provider_id.clone(), target, observed_at_ms)),
            )
            .await
            .map_err(map_environment_registry_error)?;
            targets.push(environment_target_summary_view(&target));
        }

        Ok(EnvironmentProviderHeartbeatResponse {
            provider: environment_provider_view(&provider),
            targets,
        })
    }

    pub(super) async fn unregister_environment_provider_record(
        &self,
        params: EnvironmentProviderUnregisterParams,
    ) -> Result<EnvironmentProviderUnregisterResponse, AgentApiError> {
        let provider = environment_registry::EnvironmentProviderStore::update_provider_status(
            self.store.as_ref(),
            UpdateEnvironmentProviderStatus {
                provider_id: parse_environment_provider_id(params.provider_id)?,
                status: RegistryProviderStatus::Offline,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environment_registry_error)?;

        Ok(EnvironmentProviderUnregisterResponse {
            provider: environment_provider_view(&provider),
        })
    }

    pub(super) async fn list_environment_provider_records(
        &self,
        params: EnvironmentProviderListParams,
    ) -> Result<EnvironmentProviderListResponse, AgentApiError> {
        let providers = environment_registry::EnvironmentProviderStore::list_providers(
            self.store.as_ref(),
            ListEnvironmentProviders {
                status: params.status.map(registry_provider_status),
                provider_kind: params.provider_kind.map(registry_provider_kind),
            },
        )
        .await
        .map_err(map_environment_registry_error)?
        .into_iter()
        .map(|provider| environment_provider_view(&provider))
        .collect();
        Ok(EnvironmentProviderListResponse { providers })
    }

    pub(super) async fn list_environment_provider_target_records(
        &self,
        params: EnvironmentProviderTargetListParams,
    ) -> Result<EnvironmentProviderTargetListResponse, AgentApiError> {
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        environment_registry::EnvironmentProviderStore::read_provider(
            self.store.as_ref(),
            &provider_id,
        )
        .await
        .map_err(map_environment_registry_error)?;
        let targets = environment_registry::EnvironmentTargetStore::list_targets(
            self.store.as_ref(),
            ListEnvironmentTargets {
                provider_id: Some(provider_id),
                status: params.status.map(registry_target_status),
            },
        )
        .await
        .map_err(map_environment_registry_error)?
        .into_iter()
        .map(|target| environment_target_summary_view(&target))
        .collect();
        Ok(EnvironmentProviderTargetListResponse { targets })
    }

    async fn initialize_environment_provider_controller(
        &self,
        connection: &HostControllerConnectionSpec,
    ) -> Result<(RegistryProviderCapabilities, ImplementationInfo), AgentApiError> {
        let mut controller = self.host_controller_connector.connect(connection).await?;
        let response = controller
            .initialize(&ControllerInitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "lightspeed-temporal-server".to_owned(),
            })
            .await?;
        validate_controller_protocol_version(response.protocol_version)?;
        let capabilities = RegistryProviderCapabilities::from_controller(response.capabilities);
        capabilities
            .validate()
            .map_err(map_environment_registry_error)?;
        validate_implementation(&response.implementation)?;
        Ok((capabilities, response.implementation))
    }

    async fn list_environment_provider_targets(
        &self,
        provider: &EnvironmentProviderRecord,
    ) -> Result<host_protocol::control::targets::ListTargetsResponse, AgentApiError> {
        let mut controller = self
            .host_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        controller
            .list_targets(&ListTargetsParams { status: None })
            .await
    }
}

pub(super) fn parse_environment_provider_id(
    value: String,
) -> Result<EnvironmentProviderId, AgentApiError> {
    EnvironmentProviderId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid provider id: {error}")))
}

fn registry_provider_kind(value: EnvironmentProviderKindView) -> RegistryProviderKind {
    match value {
        EnvironmentProviderKindView::Sandbox => RegistryProviderKind::Sandbox,
        EnvironmentProviderKindView::Bridge => RegistryProviderKind::Bridge,
        EnvironmentProviderKindView::Custom => RegistryProviderKind::Custom,
    }
}

fn api_provider_kind(value: RegistryProviderKind) -> EnvironmentProviderKindView {
    match value {
        RegistryProviderKind::Sandbox => EnvironmentProviderKindView::Sandbox,
        RegistryProviderKind::Bridge => EnvironmentProviderKindView::Bridge,
        RegistryProviderKind::Custom => EnvironmentProviderKindView::Custom,
    }
}

fn registry_provider_status(value: EnvironmentProviderStatusView) -> RegistryProviderStatus {
    match value {
        EnvironmentProviderStatusView::Registering => RegistryProviderStatus::Registering,
        EnvironmentProviderStatusView::Online => RegistryProviderStatus::Online,
        EnvironmentProviderStatusView::Stale => RegistryProviderStatus::Stale,
        EnvironmentProviderStatusView::Offline => RegistryProviderStatus::Offline,
        EnvironmentProviderStatusView::Disabled => RegistryProviderStatus::Disabled,
    }
}

fn api_provider_status(value: RegistryProviderStatus) -> EnvironmentProviderStatusView {
    match value {
        RegistryProviderStatus::Registering => EnvironmentProviderStatusView::Registering,
        RegistryProviderStatus::Online => EnvironmentProviderStatusView::Online,
        RegistryProviderStatus::Stale => EnvironmentProviderStatusView::Stale,
        RegistryProviderStatus::Offline => EnvironmentProviderStatusView::Offline,
        RegistryProviderStatus::Disabled => EnvironmentProviderStatusView::Disabled,
    }
}

fn registry_controller_connection(
    value: HostControllerConnectionView,
) -> Result<HostControllerConnectionSpec, AgentApiError> {
    Ok(HostControllerConnectionSpec {
        endpoint: value.endpoint,
        transport: registry_host_transport(value.transport)?,
    })
}

fn api_controller_connection(value: &HostControllerConnectionSpec) -> HostControllerConnectionView {
    HostControllerConnectionView {
        endpoint: value.endpoint.clone(),
        transport: api_host_transport(&value.transport),
    }
}

fn registry_host_transport(value: HostTransportView) -> Result<HostTransport, AgentApiError> {
    Ok(match value {
        HostTransportView::WebSocket => HostTransport::WebSocket,
        HostTransportView::Http => HostTransport::Http,
        HostTransportView::Stdio => HostTransport::Stdio,
        HostTransportView::Ssh => HostTransport::Ssh,
        HostTransportView::Provider { provider_type } => {
            if provider_type.is_empty() {
                return Err(AgentApiError::invalid_request(
                    "host transport provider type must not be empty",
                ));
            }
            HostTransport::Provider { provider_type }
        }
    })
}

fn api_host_transport(value: &HostTransport) -> HostTransportView {
    match value {
        HostTransport::WebSocket => HostTransportView::WebSocket,
        HostTransport::Http => HostTransportView::Http,
        HostTransport::Stdio => HostTransportView::Stdio,
        HostTransport::Ssh => HostTransportView::Ssh,
        HostTransport::Provider { provider_type } => HostTransportView::Provider {
            provider_type: provider_type.clone(),
        },
    }
}

fn api_provider_capabilities(
    value: RegistryProviderCapabilities,
) -> EnvironmentProviderCapabilitiesView {
    EnvironmentProviderCapabilitiesView {
        list_targets: value.list_targets,
        create_target: value.create_target,
        attach_target: value.attach_target,
        get_target: value.get_target,
        close_target: value.close_target,
    }
}

fn validate_controller_protocol_version(version: u32) -> Result<(), AgentApiError> {
    if version != CURRENT_PROTOCOL_VERSION {
        return Err(AgentApiError::rejected(format!(
            "unsupported host controller protocol version {version}; expected {CURRENT_PROTOCOL_VERSION}"
        )));
    }
    Ok(())
}

fn validate_implementation(implementation: &ImplementationInfo) -> Result<(), AgentApiError> {
    if implementation.name.is_empty() {
        return Err(AgentApiError::rejected(
            "host controller implementation name must not be empty",
        ));
    }
    if implementation
        .version
        .as_ref()
        .is_some_and(|version| version.is_empty())
    {
        return Err(AgentApiError::rejected(
            "host controller implementation version must not be empty",
        ));
    }
    Ok(())
}

fn api_implementation(value: &ImplementationInfo) -> EnvironmentProviderImplementationView {
    EnvironmentProviderImplementationView {
        name: value.name.clone(),
        version: value.version.clone(),
    }
}

fn registry_target_summary(
    value: EnvironmentTargetSummaryView,
) -> Result<HostTargetSummary, AgentApiError> {
    Ok(HostTargetSummary {
        target_id: HostTargetId::new(value.target_id),
        display_name: value.display_name,
        status: registry_target_status(value.status),
        scope: registry_host_scope(value.scope),
        capabilities: registry_host_capabilities(value.capabilities),
        default_cwd: value
            .default_cwd
            .map(HostPath::new)
            .transpose()
            .map_err(|error| {
                AgentApiError::invalid_request(format!("invalid default cwd: {error}"))
            })?,
        metadata: value.metadata,
    })
}

fn environment_provider_view(record: &EnvironmentProviderRecord) -> EnvironmentProviderView {
    EnvironmentProviderView {
        provider_id: record.provider_id.as_str().to_owned(),
        provider_kind: api_provider_kind(record.provider_kind),
        status: api_provider_status(record.status),
        controller_connection: api_controller_connection(&record.controller_connection),
        capabilities: api_provider_capabilities(record.capabilities.clone()),
        implementation: api_implementation(&record.implementation),
        last_seen_ms: record.last_seen_ms,
        lease_expires_ms: record.lease_expires_ms,
        display_name: record.display_name.clone(),
        metadata: record.metadata.clone(),
    }
}

fn environment_target_summary_view(
    record: &EnvironmentTargetRecord,
) -> EnvironmentTargetSummaryView {
    EnvironmentTargetSummaryView {
        target_id: record.target_id.as_str().to_owned(),
        status: api_target_status(record.status),
        scope: api_host_scope(&record.scope),
        capabilities: api_host_capabilities(&record.capabilities),
        display_name: record.display_name.clone(),
        default_cwd: record
            .default_cwd
            .as_ref()
            .map(|cwd| cwd.as_str().to_owned()),
        metadata: record.metadata.clone(),
    }
}

fn registry_target_status(value: EnvironmentTargetStatusView) -> HostTargetStatus {
    match value {
        EnvironmentTargetStatusView::Creating => HostTargetStatus::Creating,
        EnvironmentTargetStatusView::Starting => HostTargetStatus::Starting,
        EnvironmentTargetStatusView::Ready => HostTargetStatus::Ready,
        EnvironmentTargetStatusView::Stopped => HostTargetStatus::Stopped,
        EnvironmentTargetStatusView::Closing => HostTargetStatus::Closing,
        EnvironmentTargetStatusView::Closed => HostTargetStatus::Closed,
        EnvironmentTargetStatusView::Failed => HostTargetStatus::Failed,
        EnvironmentTargetStatusView::Unknown => HostTargetStatus::Unknown,
    }
}

fn api_target_status(value: HostTargetStatus) -> EnvironmentTargetStatusView {
    match value {
        HostTargetStatus::Creating => EnvironmentTargetStatusView::Creating,
        HostTargetStatus::Starting => EnvironmentTargetStatusView::Starting,
        HostTargetStatus::Ready => EnvironmentTargetStatusView::Ready,
        HostTargetStatus::Stopped => EnvironmentTargetStatusView::Stopped,
        HostTargetStatus::Closing => EnvironmentTargetStatusView::Closing,
        HostTargetStatus::Closed => EnvironmentTargetStatusView::Closed,
        HostTargetStatus::Failed => EnvironmentTargetStatusView::Failed,
        HostTargetStatus::Unknown => EnvironmentTargetStatusView::Unknown,
    }
}

fn registry_host_scope(value: HostScopeView) -> HostScope {
    match value {
        HostScopeView::Default => HostScope::Default,
        HostScopeView::Session { session_id } => HostScope::Session { session_id },
    }
}

fn api_host_scope(value: &HostScope) -> HostScopeView {
    match value {
        HostScope::Default => HostScopeView::Default,
        HostScope::Session { session_id } => HostScopeView::Session {
            session_id: session_id.clone(),
        },
    }
}

fn registry_host_capabilities(value: HostCapabilitiesView) -> HostCapabilities {
    HostCapabilities {
        filesystem_read: value.filesystem_read,
        filesystem_write: value.filesystem_write,
        process_start: value.process_start,
        process_stdin: value.process_stdin,
        process_terminate: value.process_terminate,
        process_output_polling: value.process_output_polling,
        process_output_notifications: value.process_output_notifications,
        process_pty: value.process_pty,
        job_start: value.job_start,
        job_list: value.job_list,
        job_read: value.job_read,
        job_cancel: value.job_cancel,
        job_wait_hint: value.job_wait_hint,
        job_dependencies: value.job_dependencies,
        job_queue_keys: value.job_queue_keys,
    }
}

fn api_host_capabilities(value: &HostCapabilities) -> HostCapabilitiesView {
    HostCapabilitiesView {
        filesystem_read: value.filesystem_read,
        filesystem_write: value.filesystem_write,
        process_start: value.process_start,
        process_stdin: value.process_stdin,
        process_terminate: value.process_terminate,
        process_output_polling: value.process_output_polling,
        process_output_notifications: value.process_output_notifications,
        process_pty: value.process_pty,
        job_start: value.job_start,
        job_list: value.job_list,
        job_read: value.job_read,
        job_cancel: value.job_cancel,
        job_wait_hint: value.job_wait_hint,
        job_dependencies: value.job_dependencies,
        job_queue_keys: value.job_queue_keys,
    }
}

pub(super) fn map_environment_registry_error(error: EnvironmentRegistryError) -> AgentApiError {
    match error {
        EnvironmentRegistryError::AlreadyExists { kind, id } => {
            AgentApiError::conflict(format!("environment registry {kind} already exists: {id}"))
        }
        EnvironmentRegistryError::NotFound { kind, id } => {
            AgentApiError::not_found(format!("environment registry {kind} not found: {id}"))
        }
        EnvironmentRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        EnvironmentRegistryError::Store { message } => AgentApiError::internal(message),
    }
}
