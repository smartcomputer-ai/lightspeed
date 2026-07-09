use super::environment_lifecycle::allocate_environment_instance_id;
use super::*;

use std::collections::BTreeSet;

use ::environments::{
    EnvironmentInstanceOrigin, EnvironmentInstanceRecord,
    EnvironmentProviderCapabilities as RegistryProviderCapabilities,
    EnvironmentProviderHeartbeat as RegistryProviderHeartbeat, EnvironmentProviderId,
    EnvironmentProviderKind as RegistryProviderKind, EnvironmentProviderPresence,
    EnvironmentProviderRecord, EnvironmentProviderStatus as RegistryProviderStatus,
    EnvironmentRegistryError, HostControllerConnectionSpec, ListEnvironmentProviders,
    ObserveEnvironmentInstance, ObservedEnvironmentTarget, RegisterEnvironmentProvider,
    UpdateEnvironmentProviderStatus,
};
use host_protocol::{
    control::{handshake::ControllerInitializeParams, targets::HostTargetStatus},
    shared::{
        CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionSpec, HostPath, HostScope,
        HostTargetId, HostTransport, ImplementationInfo,
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
        let provider = ::environments::EnvironmentProviderStore::register_provider(
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
        .map_err(map_environments_error)?;
        Ok(EnvironmentProviderRegisterResponse {
            provider: environment_provider_view(&provider, observed_at_ms),
        })
    }

    pub(super) async fn heartbeat_environment_provider_record(
        &self,
        params: EnvironmentProviderHeartbeatParams,
    ) -> Result<EnvironmentProviderHeartbeatResponse, AgentApiError> {
        let observed_at_ms = now_ms()?;
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let observations = params
            .observed_targets
            .into_iter()
            .map(registry_target_descriptor)
            .collect::<Result<Vec<_>, _>>()?;
        let provider = ::environments::EnvironmentProviderStore::update_provider_heartbeat(
            self.store.as_ref(),
            RegistryProviderHeartbeat {
                provider_id: provider_id.clone(),
                observed_at_ms,
                lease_ttl_ms: params.lease_ttl_ms,
                observed_targets: observations.clone(),
            },
        )
        .await
        .map_err(map_environments_error)?;

        let mut observed_ids = BTreeSet::new();
        let mut instances = Vec::with_capacity(observations.len());
        for observation in observations {
            observed_ids.insert(observation.target.target_id.clone());
            let instance_id =
                match ::environments::EnvironmentInstanceStore::read_instance_by_provider_target(
                    self.store.as_ref(),
                    &provider_id,
                    &observation.target.target_id,
                )
                .await
                {
                    Ok(instance) => instance.instance_id,
                    Err(EnvironmentRegistryError::NotFound { .. }) => {
                        allocate_environment_instance_id()
                    }
                    Err(error) => return Err(map_environments_error(error)),
                };
            let instance = ::environments::EnvironmentInstanceStore::observe_instance(
                self.store.as_ref(),
                ObserveEnvironmentInstance::from_observation(
                    instance_id,
                    provider_id.clone(),
                    EnvironmentInstanceOrigin::Provided,
                    observation,
                    observed_at_ms,
                ),
            )
            .await
            .map_err(map_environments_error)?;
            instances.push(environment_instance_view(&instance));
        }
        ::environments::EnvironmentInstanceStore::mark_missing_provided_instances_unknown(
            self.store.as_ref(),
            &provider_id,
            &observed_ids,
            observed_at_ms,
        )
        .await
        .map_err(map_environments_error)?;

        Ok(EnvironmentProviderHeartbeatResponse {
            provider: environment_provider_view(&provider, observed_at_ms),
            environments: instances,
        })
    }

    pub(super) async fn unregister_environment_provider_record(
        &self,
        params: EnvironmentProviderUnregisterParams,
    ) -> Result<EnvironmentProviderUnregisterResponse, AgentApiError> {
        let now = now_ms()?;
        let provider = ::environments::EnvironmentProviderStore::update_provider_status(
            self.store.as_ref(),
            UpdateEnvironmentProviderStatus {
                provider_id: parse_environment_provider_id(params.provider_id)?,
                status: RegistryProviderStatus::Offline,
                updated_at_ms: now,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentProviderUnregisterResponse {
            provider: environment_provider_view(&provider, now),
        })
    }

    pub(super) async fn list_environment_provider_records(
        &self,
        params: EnvironmentProviderListParams,
    ) -> Result<EnvironmentProviderListResponse, AgentApiError> {
        let now = now_ms()?;
        let providers = ::environments::EnvironmentProviderStore::list_providers(
            self.store.as_ref(),
            ListEnvironmentProviders {
                status: None,
                provider_kind: params.provider_kind.map(registry_provider_kind),
            },
        )
        .await
        .map_err(map_environments_error)?
        .into_iter()
        .filter(|provider| {
            params
                .status
                .is_none_or(|status| api_provider_presence(provider.presence_at(now)) == status)
        })
        .map(|provider| environment_provider_view(&provider, now))
        .collect();
        Ok(EnvironmentProviderListResponse { providers })
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
        if response.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(AgentApiError::rejected(format!(
                "unsupported host controller protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                response.protocol_version
            )));
        }
        if response.implementation.name.is_empty() {
            return Err(AgentApiError::rejected(
                "host controller implementation name must not be empty",
            ));
        }
        let capabilities = RegistryProviderCapabilities::from_controller(response.capabilities);
        capabilities.validate().map_err(map_environments_error)?;
        Ok((capabilities, response.implementation))
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

fn api_provider_presence(value: EnvironmentProviderPresence) -> EnvironmentProviderStatusView {
    match value {
        EnvironmentProviderPresence::Online => EnvironmentProviderStatusView::Online,
        EnvironmentProviderPresence::Stale => EnvironmentProviderStatusView::Stale,
        EnvironmentProviderPresence::Offline => EnvironmentProviderStatusView::Offline,
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

pub(super) fn registry_host_transport(
    value: HostTransportView,
) -> Result<HostTransport, AgentApiError> {
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

pub(super) fn api_host_transport(value: &HostTransport) -> HostTransportView {
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

fn registry_target_descriptor(
    value: EnvironmentTargetDescriptorView,
) -> Result<ObservedEnvironmentTarget, AgentApiError> {
    let target_id = HostTargetId::new(value.target.target_id);
    let connection = registry_host_connection(value.connection)?;
    if target_id != connection.target_id {
        return Err(AgentApiError::invalid_request(
            "heartbeat target and connection target ids must match",
        ));
    }
    Ok(ObservedEnvironmentTarget {
        target: host_protocol::control::targets::HostTargetSummary {
            target_id,
            display_name: value.target.display_name,
            status: registry_target_status(value.target.status),
            scope: registry_host_scope(value.target.scope),
            capabilities: registry_host_capabilities(value.target.capabilities),
            default_cwd: optional_host_path(value.target.default_cwd, "default cwd")?,
            metadata: value.target.metadata,
        },
        connection,
    })
}

fn registry_host_connection(
    value: HostConnectionView,
) -> Result<HostConnectionSpec, AgentApiError> {
    Ok(HostConnectionSpec {
        target_id: HostTargetId::new(value.target_id),
        endpoint: value.endpoint,
        transport: registry_host_transport(value.transport)?,
        scope: registry_host_scope(value.scope),
        default_cwd: optional_host_path(value.default_cwd, "connection default cwd")?,
        capabilities: registry_host_capabilities(value.capabilities),
    })
}

fn optional_host_path(
    value: Option<String>,
    name: &str,
) -> Result<Option<HostPath>, AgentApiError> {
    value
        .map(HostPath::new)
        .transpose()
        .map_err(|error| AgentApiError::invalid_request(format!("invalid {name}: {error}")))
}

fn environment_provider_view(
    record: &EnvironmentProviderRecord,
    now_ms: i64,
) -> EnvironmentProviderView {
    EnvironmentProviderView {
        provider_id: record.provider_id.as_str().to_owned(),
        provider_kind: api_provider_kind(record.provider_kind),
        status: api_provider_presence(record.presence_at(now_ms)),
        controller_connection: api_controller_connection(&record.controller_connection),
        capabilities: EnvironmentProviderCapabilitiesView {
            list_targets: record.capabilities.list_targets,
            create_target: record.capabilities.create_target,
            get_target: record.capabilities.get_target,
            close_target: record.capabilities.close_target,
        },
        implementation: EnvironmentProviderImplementationView {
            name: record.implementation.name.clone(),
            version: record.implementation.version.clone(),
        },
        last_seen_ms: record.last_seen_ms,
        lease_expires_ms: record.lease_expires_ms,
        display_name: record.display_name.clone(),
        metadata: record.metadata.clone(),
    }
}

pub(super) fn environment_instance_view(
    record: &EnvironmentInstanceRecord,
) -> EnvironmentInstanceView {
    EnvironmentInstanceView {
        instance_id: record.instance_id.as_str().to_owned(),
        provider_id: record.provider_id.as_str().to_owned(),
        provider_target_id: record.provider_target_id.as_str().to_owned(),
        origin: match record.origin {
            EnvironmentInstanceOrigin::Provided => EnvironmentInstanceOriginView::Provided,
            EnvironmentInstanceOrigin::Provisioned => EnvironmentInstanceOriginView::Provisioned,
        },
        status: api_target_status(record.status),
        scope: api_host_scope(&record.scope),
        capabilities: api_host_capabilities(&record.capabilities),
        connection: api_host_connection(&record.connection),
        default_cwd: record
            .default_cwd
            .as_ref()
            .map(|path| path.as_str().to_owned()),
        metadata: record.metadata.clone(),
        observed_at_ms: record.observed_at_ms,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn api_host_connection(value: &HostConnectionSpec) -> HostConnectionView {
    HostConnectionView {
        target_id: value.target_id.as_str().to_owned(),
        endpoint: value.endpoint.clone(),
        transport: api_host_transport(&value.transport),
        scope: api_host_scope(&value.scope),
        default_cwd: value
            .default_cwd
            .as_ref()
            .map(|path| path.as_str().to_owned()),
        capabilities: api_host_capabilities(&value.capabilities),
    }
}

pub(super) fn registry_target_status(value: EnvironmentTargetStatusView) -> HostTargetStatus {
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
        network: value.network,
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
        network: value.network,
    }
}

pub(super) fn map_environments_error(error: EnvironmentRegistryError) -> AgentApiError {
    match error {
        EnvironmentRegistryError::AlreadyExists { kind, id } => {
            AgentApiError::conflict(format!("environment registry {kind} already exists: {id}"))
        }
        EnvironmentRegistryError::NotFound { kind, id } => {
            AgentApiError::not_found(format!("environment registry {kind} not found: {id}"))
        }
        EnvironmentRegistryError::Occupied {
            instance_id,
            bindings,
            job_groups,
        } => AgentApiError::conflict(format!(
            "environment instance {instance_id} is occupied by bindings {bindings:?} and job groups {job_groups:?}"
        )),
        EnvironmentRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        EnvironmentRegistryError::Store { message } => AgentApiError::internal(message),
    }
}
