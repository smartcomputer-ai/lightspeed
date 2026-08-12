use super::*;

use ::environments::{
    EnvironmentProviderBindingId, EnvironmentProviderBindingRecord,
    EnvironmentProviderBindingStatus, EnvironmentProviderId, EnvironmentProviderRecord,
    EnvironmentRecord, EnvironmentRegistryError, EnvironmentSource, EnvironmentStatus,
};
use host_protocol::control::targets::{
    EnvironmentTemplate, ListTemplatesParams, ProviderBindingContext,
};

impl GatewayAgentApi {
    pub(super) async fn list_environment_provider_binding_records(
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

    pub(super) async fn read_environment_provider_binding_record(
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

    pub(super) async fn list_environment_template_records(
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
                .host_controller_connector
                .connect(&provider.controller_connection)
                .await?;
            let response = controller
                .list_templates(&ListTemplatesParams {
                    binding: binding_context(&binding),
                })
                .await?;
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

    pub(super) async fn read_environment_template_record(
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

    pub(super) async fn read_environment_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, AgentApiError> {
        ::environments::EnvironmentProviderStore::read_provider(self.store.as_ref(), provider_id)
            .await
            .map_err(map_environments_error)
    }
}

pub(super) fn binding_context(record: &EnvironmentProviderBindingRecord) -> ProviderBindingContext {
    ProviderBindingContext {
        universe_id: record.universe_id.to_string(),
        binding_id: record.binding_id.to_string(),
    }
}

pub(super) fn parse_environment_provider_id(
    value: String,
) -> Result<EnvironmentProviderId, AgentApiError> {
    EnvironmentProviderId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid provider id: {error}")))
}

pub(super) fn parse_environment_provider_binding_id(
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

pub(super) fn environment_template_view(
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

pub(super) fn environment_view(record: &EnvironmentRecord) -> EnvironmentView {
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
            EnvironmentSource::Enrolled => EnvironmentSourceView::Enrolled,
        },
        display_name: record.display_name.clone(),
        status: lifecycle_status_view(record.status),
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
            created_at_ms: record.incarnation.created_at_ms,
            updated_at_ms: record.incarnation.updated_at_ms,
        },
        metadata: record.metadata.clone(),
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn lifecycle_status_view(value: EnvironmentStatus) -> EnvironmentLifecycleStatusView {
    match value {
        EnvironmentStatus::Provisioning => EnvironmentLifecycleStatusView::Provisioning,
        EnvironmentStatus::Booting => EnvironmentLifecycleStatusView::Booting,
        EnvironmentStatus::WaitingForDaemon => EnvironmentLifecycleStatusView::WaitingForDaemon,
        EnvironmentStatus::Ready => EnvironmentLifecycleStatusView::Ready,
        EnvironmentStatus::Offline => EnvironmentLifecycleStatusView::Offline,
        EnvironmentStatus::Closing => EnvironmentLifecycleStatusView::Closing,
        EnvironmentStatus::Closed => EnvironmentLifecycleStatusView::Closed,
        EnvironmentStatus::Failed => EnvironmentLifecycleStatusView::Failed,
        EnvironmentStatus::Unknown => EnvironmentLifecycleStatusView::Unknown,
    }
}

pub(super) fn registry_lifecycle_status(
    value: EnvironmentLifecycleStatusView,
) -> EnvironmentStatus {
    match value {
        EnvironmentLifecycleStatusView::Provisioning => EnvironmentStatus::Provisioning,
        EnvironmentLifecycleStatusView::Booting => EnvironmentStatus::Booting,
        EnvironmentLifecycleStatusView::WaitingForDaemon => EnvironmentStatus::WaitingForDaemon,
        EnvironmentLifecycleStatusView::Ready => EnvironmentStatus::Ready,
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
        EnvironmentRegistryError::Store { message } => AgentApiError::internal(message),
    }
}
