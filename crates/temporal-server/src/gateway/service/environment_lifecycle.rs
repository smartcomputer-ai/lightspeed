use super::environment_providers::{
    binding_context, environment_view, map_environments_error,
    parse_environment_provider_binding_id, registry_lifecycle_status,
};
use super::*;

use ::environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment, EnvironmentId,
    EnvironmentIncarnationId, EnvironmentProviderBindingStore, EnvironmentProvisionRequestId,
    EnvironmentSource, EnvironmentStatus, EnvironmentStore, EnvironmentTemplateId,
    FailEnvironmentLifecycle, FinishCloseEnvironment, ListEnvironments,
    ObserveProvisionedEnvironment,
};
use host_protocol::control::targets::{CloseTargetParams, CreateTargetParams, HostTargetStatus};

impl GatewayAgentApi {
    pub(super) async fn create_external_environment_record(
        &self,
        params: EnvironmentExternalCreateParams,
    ) -> Result<EnvironmentExternalCreateResponse, AgentApiError> {
        let request_id =
            EnvironmentProvisionRequestId::try_new(params.request_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid environment request id: {error}"))
            })?;
        let connection = external_connection_from_api(params.connection)?;
        let environment = EnvironmentStore::create_external_environment(
            self.store.as_ref(),
            CreateExternalEnvironment {
                request_id,
                environment_id: allocate_environment_id(),
                incarnation_id: allocate_incarnation_id(),
                connection,
                display_name: params.display_name,
                metadata: params.metadata,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentExternalCreateResponse {
            environment: environment_view(&environment),
        })
    }
    pub(super) async fn create_environment_record(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentCreateResponse, AgentApiError> {
        let request_id =
            EnvironmentProvisionRequestId::try_new(params.request_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid environment request id: {error}"))
            })?;
        let binding_id = parse_environment_provider_binding_id(params.binding_id)?;
        let template_id = EnvironmentTemplateId::try_new(params.template_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid environment template id: {error}"))
        })?;
        let environment = EnvironmentStore::create_environment(
            self.store.as_ref(),
            CreateEnvironment {
                request_id,
                environment_id: allocate_environment_id(),
                incarnation_id: allocate_incarnation_id(),
                binding_id,
                template_id,
                display_name: params.display_name,
                metadata: params.metadata,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        // The transaction above is the acceptance boundary. Provider I/O is
        // deliberately left to the independently restartable reconciler.
        Ok(EnvironmentCreateResponse {
            environment: environment_view(&environment),
        })
    }

    pub(super) async fn read_environment_record(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<EnvironmentReadResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let environment = EnvironmentStore::read_environment(self.store.as_ref(), &environment_id)
            .await
            .map_err(map_environments_error)?;
        Ok(EnvironmentReadResponse {
            environment: environment_view(&environment),
        })
    }

    pub(super) async fn list_environment_records(
        &self,
        params: EnvironmentListParams,
    ) -> Result<EnvironmentListResponse, AgentApiError> {
        let environments = EnvironmentStore::list_environments(
            self.store.as_ref(),
            ListEnvironments {
                provider_id: params
                    .provider_id
                    .map(parse_environment_provider_id)
                    .transpose()?,
                binding_id: params
                    .binding_id
                    .map(parse_environment_provider_binding_id)
                    .transpose()?,
                status: params.status.map(registry_lifecycle_status),
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentListResponse {
            environments: environments.iter().map(environment_view).collect(),
        })
    }

    pub(super) async fn close_environment_record(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<EnvironmentCloseResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let environment = EnvironmentStore::begin_close_environment(
            self.store.as_ref(),
            BeginCloseEnvironment {
                environment_id,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentCloseResponse {
            environment: environment_view(&environment),
        })
    }

    /// Reconcile every pending environment in this universe once. Calls are
    /// stable and idempotent; a crash after controller I/O is recovered by a
    /// later pass issuing the same IDs again.
    pub(crate) async fn reconcile_environment_lifecycle_once(
        &self,
    ) -> Result<usize, AgentApiError> {
        let environments =
            EnvironmentStore::list_environments_needing_reconcile(self.store.as_ref())
                .await
                .map_err(map_environments_error)?;
        let mut changed = 0;
        for environment in environments {
            match environment.status {
                EnvironmentStatus::Closing => {
                    if self.reconcile_environment_close(&environment).await? {
                        changed += 1;
                    }
                }
                EnvironmentStatus::Provisioning
                | EnvironmentStatus::Booting
                | EnvironmentStatus::Unknown => {
                    match self.reconcile_environment_create(&environment).await {
                        Ok(did_change) => changed += usize::from(did_change),
                        Err(error) if error.kind == api::AgentApiErrorKind::Rejected => {
                            EnvironmentStore::fail_environment_lifecycle(
                                self.store.as_ref(),
                                FailEnvironmentLifecycle {
                                    environment_id: environment.environment_id.clone(),
                                    message: error.message,
                                    observed_at_ms: now_ms()?,
                                },
                            )
                            .await
                            .map_err(map_environments_error)?;
                            changed += 1;
                        }
                        Err(error) => return Err(error),
                    }
                }
                _ => {}
            }
        }
        Ok(changed)
    }

    async fn reconcile_environment_create(
        &self,
        environment: &::environments::EnvironmentRecord,
    ) -> Result<bool, AgentApiError> {
        let EnvironmentSource::Provisioned {
            provider_id,
            binding_id,
        } = &environment.source
        else {
            return Ok(false);
        };
        let provider = self.read_environment_provider(provider_id).await?;
        let binding = EnvironmentProviderBindingStore::read_provider_binding(
            self.store.as_ref(),
            self.store.config().universe_id,
            binding_id,
        )
        .await
        .map_err(map_environments_error)?;
        let template_id = environment
            .incarnation
            .template_id
            .as_ref()
            .ok_or_else(|| AgentApiError::internal("provisioned incarnation has no template id"))?;
        let mut controller = self
            .host_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = controller
            .create_target(&CreateTargetParams {
                request_id: environment.request_id.to_string(),
                environment_id: environment.environment_id.to_string(),
                incarnation_id: environment.incarnation.incarnation_id.to_string(),
                binding: binding_context(&binding),
                template_id: template_id.to_string(),
            })
            .await?;
        let status = match response.target.status {
            // A passive provider reports Ready only after its private envd is
            // reachable. No provider presence has to register separately.
            HostTargetStatus::Ready => EnvironmentStatus::Ready,
            HostTargetStatus::Creating | HostTargetStatus::Starting => EnvironmentStatus::Booting,
            HostTargetStatus::Stopped => EnvironmentStatus::Offline,
            HostTargetStatus::Closing => EnvironmentStatus::Closing,
            HostTargetStatus::Closed => EnvironmentStatus::Closed,
            HostTargetStatus::Failed => EnvironmentStatus::Failed,
            HostTargetStatus::Unknown => EnvironmentStatus::Unknown,
        };
        let changed = environment.status != status
            || environment.incarnation.provider_target_id.as_ref()
                != Some(&response.target.target_id);
        if changed {
            EnvironmentStore::observe_provisioned_environment(
                self.store.as_ref(),
                ObserveProvisionedEnvironment {
                    environment_id: environment.environment_id.clone(),
                    provider_target_id: response.target.target_id,
                    status,
                    observed_at_ms: now_ms()?,
                },
            )
            .await
            .map_err(map_environments_error)?;
        }
        Ok(changed)
    }

    async fn reconcile_environment_close(
        &self,
        environment: &::environments::EnvironmentRecord,
    ) -> Result<bool, AgentApiError> {
        let Some(target_id) = environment.incarnation.provider_target_id.clone() else {
            EnvironmentStore::finish_close_environment(
                self.store.as_ref(),
                FinishCloseEnvironment {
                    environment_id: environment.environment_id.clone(),
                    observed_at_ms: now_ms()?,
                },
            )
            .await
            .map_err(map_environments_error)?;
            return Ok(true);
        };
        let EnvironmentSource::Provisioned {
            provider_id,
            binding_id,
        } = &environment.source
        else {
            return Ok(false);
        };
        let provider = self.read_environment_provider(provider_id).await?;
        let binding = EnvironmentProviderBindingStore::read_provider_binding(
            self.store.as_ref(),
            self.store.config().universe_id,
            binding_id,
        )
        .await
        .map_err(map_environments_error)?;
        let mut controller = self
            .host_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = controller
            .close_target(&CloseTargetParams {
                request_id: format!("close:{}", environment.request_id),
                environment_id: environment.environment_id.to_string(),
                incarnation_id: environment.incarnation.incarnation_id.to_string(),
                binding: binding_context(&binding),
                target_id,
                force: false,
            })
            .await?;
        if response.status == HostTargetStatus::Closed {
            EnvironmentStore::finish_close_environment(
                self.store.as_ref(),
                FinishCloseEnvironment {
                    environment_id: environment.environment_id.clone(),
                    observed_at_ms: now_ms()?,
                },
            )
            .await
            .map_err(map_environments_error)?;
        }
        Ok(true)
    }
}

fn external_connection_from_api(
    value: EnvironmentConnectionView,
) -> Result<::environments::EnvironmentConnectionSpec, AgentApiError> {
    let transport = match value.transport {
        EnvironmentConnectionTransportView::WebSocket => {
            host_protocol::shared::HostTransport::WebSocket
        }
        _ => {
            return Err(AgentApiError::invalid_request(
                "external environment connections currently require webSocket transport",
            ));
        }
    };
    Ok(::environments::EnvironmentConnectionSpec {
        endpoint: value.endpoint,
        transport,
    })
}

pub(super) fn parse_registry_environment_id(value: String) -> Result<EnvironmentId, AgentApiError> {
    EnvironmentId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid environment id: {error}")))
}

pub(super) fn allocate_environment_id() -> EnvironmentId {
    EnvironmentId::new(format!("environment_{}", uuid::Uuid::new_v4().simple()))
}

fn allocate_incarnation_id() -> EnvironmentIncarnationId {
    EnvironmentIncarnationId::new(format!("incarnation_{}", uuid::Uuid::new_v4().simple()))
}
