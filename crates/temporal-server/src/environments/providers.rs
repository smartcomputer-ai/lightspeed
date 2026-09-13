use super::*;

use ::environments::{
    EnvironmentProviderBindingId, EnvironmentProviderBindingRecord,
    EnvironmentProviderBindingStatus, EnvironmentProviderId, EnvironmentProviderRecord,
    EnvironmentRecord, EnvironmentRegistryError, EnvironmentSource, EnvironmentStatus,
};
use environment_protocol::control::targets::{
    EnvironmentTemplate, ListTemplatesParams, ProviderBindingContext,
};

impl EnvironmentService {
    pub(crate) async fn list_environment_provider_binding_records(
        &self,
        _params: EnvironmentProviderBindingListParams,
    ) -> Result<EnvironmentProviderBindingListResponse, AgentApiError> {
        let records = ::environments::EnvironmentProviderBindingStore::list_provider_bindings(
            self.store.as_ref(),
            self.store.config().universe_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentProviderBindingListResponse {
            bindings: records
                .iter()
                .map(environment_provider_binding_view)
                .collect(),
        })
    }

    pub(crate) async fn read_environment_provider_binding_record(
        &self,
        params: EnvironmentProviderBindingReadParams,
    ) -> Result<EnvironmentProviderBindingReadResponse, AgentApiError> {
        let binding_id = parse_environment_provider_binding_id(params.binding_id)?;
        let record = ::environments::EnvironmentProviderBindingStore::read_provider_binding(
            self.store.as_ref(),
            self.store.config().universe_id,
            &binding_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentProviderBindingReadResponse {
            binding: environment_provider_binding_view(&record),
        })
    }

    pub(crate) async fn list_environment_template_records(
        &self,
        params: EnvironmentTemplateListParams,
    ) -> Result<EnvironmentTemplateListResponse, AgentApiError> {
        let binding_filter = params
            .binding_id
            .map(parse_environment_provider_binding_id)
            .transpose()?;
        let bindings = if let Some(binding_id) = binding_filter {
            vec![
                ::environments::EnvironmentProviderBindingStore::read_provider_binding(
                    self.store.as_ref(),
                    self.store.config().universe_id,
                    &binding_id,
                )
                .await
                .map_err(map_environments_error)?,
            ]
        } else {
            ::environments::EnvironmentProviderBindingStore::list_provider_bindings(
                self.store.as_ref(),
                self.store.config().universe_id,
            )
            .await
            .map_err(map_environments_error)?
        };
        let mut templates = Vec::new();
        for binding in bindings
            .into_iter()
            .filter(|binding| binding.status == EnvironmentProviderBindingStatus::Enabled)
        {
            let provider = self.read_environment_provider(&binding.provider_id).await?;
            let mut controller = self
                .provider_controller_connector
                .connect(&provider.controller_connection)
                .await?;
            let response = controller
                .list_templates(&ListTemplatesParams {
                    binding: binding_context(&binding),
                })
                .await;
            let response = finish_provider_controller(controller, response).await?;
            templates.extend(
                response
                    .templates
                    .iter()
                    .map(|template| environment_template_view(&binding, template)),
            );
        }
        templates.sort_by(|left, right| {
            (&left.binding_id, &left.template_id).cmp(&(&right.binding_id, &right.template_id))
        });
        Ok(EnvironmentTemplateListResponse { templates })
    }

    pub(crate) async fn read_environment_template_record(
        &self,
        params: EnvironmentTemplateReadParams,
    ) -> Result<EnvironmentTemplateReadResponse, AgentApiError> {
        let expected = params.template_id;
        let response = self
            .list_environment_template_records(EnvironmentTemplateListParams {
                binding_id: Some(params.binding_id),
            })
            .await?;
        let template = response
            .templates
            .into_iter()
            .find(|template| template.template_id == expected)
            .ok_or_else(|| {
                AgentApiError::not_found(format!("environment template not found: {expected}"))
            })?;
        Ok(EnvironmentTemplateReadResponse { template })
    }

    pub(crate) async fn read_environment_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, AgentApiError> {
        ::environments::EnvironmentProviderStore::read_provider(self.store.as_ref(), provider_id)
            .await
            .map_err(map_environments_error)
    }
}

pub(crate) fn binding_context(record: &EnvironmentProviderBindingRecord) -> ProviderBindingContext {
    ProviderBindingContext {
        universe_id: record.universe_id.to_string(),
        binding_id: record.binding_id.to_string(),
    }
}

pub(crate) fn parse_environment_provider_id(
    value: String,
) -> Result<EnvironmentProviderId, AgentApiError> {
    EnvironmentProviderId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid provider id: {error}")))
}

pub(crate) fn parse_environment_provider_binding_id(
    value: String,
) -> Result<EnvironmentProviderBindingId, AgentApiError> {
    EnvironmentProviderBindingId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid provider binding id: {error}"))
    })
}

pub(crate) fn environment_provider_binding_view(
    record: &EnvironmentProviderBindingRecord,
) -> EnvironmentProviderBindingView {
    EnvironmentProviderBindingView {
        binding_id: record.binding_id.to_string(),
        provider_id: record.provider_id.to_string(),
        status: match record.status {
            EnvironmentProviderBindingStatus::Enabled => {
                EnvironmentProviderBindingStatusView::Enabled
            }
            EnvironmentProviderBindingStatus::Disabled => {
                EnvironmentProviderBindingStatusView::Disabled
            }
        },
        revision: record.revision,
        metadata: record.metadata.clone(),
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(crate) fn environment_template_view(
    binding: &EnvironmentProviderBindingRecord,
    template: &EnvironmentTemplate,
) -> EnvironmentTemplateView {
    EnvironmentTemplateView {
        template_id: template.template_id.clone(),
        provider_id: binding.provider_id.to_string(),
        binding_id: binding.binding_id.to_string(),
        display_name: template.display_name.clone(),
        description: template.description.clone(),
        public_ingress: template.public_ingress,
        deprecated: template.deprecated,
        metadata: template.metadata.clone(),
    }
}

pub(crate) fn environment_view(record: &EnvironmentRecord) -> EnvironmentView {
    EnvironmentView {
        environment_id: record.environment_id.to_string(),
        request_id: record.request_id.to_string(),
        source: match &record.source {
            EnvironmentSource::Provisioned {
                provider_id,
                binding_id,
            } => EnvironmentSourceView::Provisioned {
                provider_id: provider_id.to_string(),
                binding_id: binding_id.to_string(),
            },
            EnvironmentSource::External { connection } => EnvironmentSourceView::External {
                connection: EnvironmentConnectionView {
                    endpoint: connection.endpoint.clone(),
                    transport: match &connection.transport {
                        environment_protocol::shared::EnvironmentTransport::WebSocket => {
                            EnvironmentConnectionTransportView::WebSocket
                        }
                        environment_protocol::shared::EnvironmentTransport::Http => {
                            EnvironmentConnectionTransportView::Http
                        }
                        environment_protocol::shared::EnvironmentTransport::Stdio => {
                            EnvironmentConnectionTransportView::Stdio
                        }
                        environment_protocol::shared::EnvironmentTransport::Ssh => {
                            EnvironmentConnectionTransportView::Ssh
                        }
                        environment_protocol::shared::EnvironmentTransport::Provider {
                            provider_type,
                        } => EnvironmentConnectionTransportView::Provider {
                            provider_type: provider_type.clone(),
                        },
                    },
                },
            },
            EnvironmentSource::Registered {
                registration_key_id,
                daemon_id,
                identity_mode,
                ..
            } => EnvironmentSourceView::Registered {
                registration_key_id: registration_key_id.to_string(),
                daemon_id: daemon_id.to_string(),
                identity_mode: identity_mode_view(*identity_mode),
            },
        },
        display_name: record.display_name.clone(),
        status: lifecycle_status_view(record.status),
        desired_power: power_state_view(record.desired_power),
        idle_policy: record.idle_policy.as_ref().map(idle_policy_view),
        incarnation: EnvironmentIncarnationView {
            incarnation_id: record.incarnation.incarnation_id.to_string(),
            provision_request_id: record
                .incarnation
                .provision_request_id
                .as_ref()
                .map(ToString::to_string),
            provider_target_id: record
                .incarnation
                .provider_target_id
                .as_ref()
                .map(ToString::to_string),
            template_id: record
                .incarnation
                .template_id
                .as_ref()
                .map(ToString::to_string),
            power_states: record
                .incarnation
                .power_states
                .iter()
                .copied()
                .map(power_state_view)
                .collect(),
            created_at_ms: record.incarnation.created_at_ms,
            updated_at_ms: record.incarnation.updated_at_ms,
        },
        public_ingress_enabled: record.public_ingress_enabled,
        public_endpoint: record.public_endpoint.clone(),
        metadata: record.metadata.clone(),
        last_seen_at_ms: record.last_seen_at_ms,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(crate) fn identity_mode_view(
    value: ::environments::RegisteredIdentityMode,
) -> api::EnvironmentIdentityModeView {
    match value {
        ::environments::RegisteredIdentityMode::Persistent => {
            api::EnvironmentIdentityModeView::Persistent
        }
        ::environments::RegisteredIdentityMode::Ephemeral => {
            api::EnvironmentIdentityModeView::Ephemeral
        }
    }
}

pub(crate) fn registry_identity_mode(
    value: api::EnvironmentIdentityModeView,
) -> ::environments::RegisteredIdentityMode {
    match value {
        api::EnvironmentIdentityModeView::Persistent => {
            ::environments::RegisteredIdentityMode::Persistent
        }
        api::EnvironmentIdentityModeView::Ephemeral => {
            ::environments::RegisteredIdentityMode::Ephemeral
        }
    }
}

pub(crate) fn power_state_view(value: ::environments::PowerState) -> EnvironmentPowerStateView {
    match value {
        ::environments::PowerState::Running => EnvironmentPowerStateView::Running,
        ::environments::PowerState::Paused => EnvironmentPowerStateView::Paused,
        ::environments::PowerState::Suspended => EnvironmentPowerStateView::Suspended,
        ::environments::PowerState::Stopped => EnvironmentPowerStateView::Stopped,
    }
}

pub(crate) fn registry_power_state(value: EnvironmentPowerStateView) -> ::environments::PowerState {
    match value {
        EnvironmentPowerStateView::Running => ::environments::PowerState::Running,
        EnvironmentPowerStateView::Paused => ::environments::PowerState::Paused,
        EnvironmentPowerStateView::Suspended => ::environments::PowerState::Suspended,
        EnvironmentPowerStateView::Stopped => ::environments::PowerState::Stopped,
    }
}

pub(crate) fn idle_policy_view(
    value: &::environments::EnvironmentIdlePolicy,
) -> EnvironmentIdlePolicyView {
    EnvironmentIdlePolicyView {
        pause_after_ms: value.pause_after_ms,
        suspend_after_ms: value.suspend_after_ms,
        stop_after_ms: value.stop_after_ms,
        close_after_ms: value.close_after_ms,
    }
}

pub(crate) fn registry_idle_policy(
    value: &EnvironmentIdlePolicyView,
) -> ::environments::EnvironmentIdlePolicy {
    ::environments::EnvironmentIdlePolicy {
        pause_after_ms: value.pause_after_ms,
        suspend_after_ms: value.suspend_after_ms,
        stop_after_ms: value.stop_after_ms,
        close_after_ms: value.close_after_ms,
    }
}

fn lifecycle_status_view(value: EnvironmentStatus) -> EnvironmentLifecycleStatusView {
    match value {
        EnvironmentStatus::Provisioning => EnvironmentLifecycleStatusView::Provisioning,
        EnvironmentStatus::Booting => EnvironmentLifecycleStatusView::Booting,
        EnvironmentStatus::Ready => EnvironmentLifecycleStatusView::Ready,
        EnvironmentStatus::Paused => EnvironmentLifecycleStatusView::Paused,
        EnvironmentStatus::Suspended => EnvironmentLifecycleStatusView::Suspended,
        EnvironmentStatus::Offline => EnvironmentLifecycleStatusView::Offline,
        EnvironmentStatus::Closing => EnvironmentLifecycleStatusView::Closing,
        EnvironmentStatus::Closed => EnvironmentLifecycleStatusView::Closed,
        EnvironmentStatus::Failed => EnvironmentLifecycleStatusView::Failed,
        EnvironmentStatus::Unknown => EnvironmentLifecycleStatusView::Unknown,
    }
}

pub(crate) fn registry_lifecycle_status(
    value: EnvironmentLifecycleStatusView,
) -> EnvironmentStatus {
    match value {
        EnvironmentLifecycleStatusView::Provisioning => EnvironmentStatus::Provisioning,
        EnvironmentLifecycleStatusView::Booting => EnvironmentStatus::Booting,
        EnvironmentLifecycleStatusView::Ready => EnvironmentStatus::Ready,
        EnvironmentLifecycleStatusView::Paused => EnvironmentStatus::Paused,
        EnvironmentLifecycleStatusView::Suspended => EnvironmentStatus::Suspended,
        EnvironmentLifecycleStatusView::Offline => EnvironmentStatus::Offline,
        EnvironmentLifecycleStatusView::Closing => EnvironmentStatus::Closing,
        EnvironmentLifecycleStatusView::Closed => EnvironmentStatus::Closed,
        EnvironmentLifecycleStatusView::Failed => EnvironmentStatus::Failed,
        EnvironmentLifecycleStatusView::Unknown => EnvironmentStatus::Unknown,
    }
}

pub(crate) fn map_environments_error(error: EnvironmentRegistryError) -> AgentApiError {
    match error {
        EnvironmentRegistryError::AlreadyExists { kind, id } => {
            AgentApiError::conflict(format!("environment registry {kind} already exists: {id}"))
        }
        EnvironmentRegistryError::NotFound { kind, id } => {
            AgentApiError::not_found(format!("environment registry {kind} not found: {id}"))
        }
        EnvironmentRegistryError::RevisionConflict {
            kind,
            id,
            expected,
            actual,
        } => AgentApiError::conflict(format!(
            "environment registry revision conflict for {kind} {id}: expected {expected:?}, actual {actual:?}"
        )),
        EnvironmentRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        refused @ EnvironmentRegistryError::RegistrationKeyUnavailable { .. } => {
            AgentApiError::rejected(refused.to_string())
        }
        refused @ EnvironmentRegistryError::RegistrationCapacityExhausted { .. } => {
            AgentApiError::rejected(refused.to_string())
        }
        EnvironmentRegistryError::Store { message } => AgentApiError::internal(message),
    }
}
