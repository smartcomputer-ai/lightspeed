use super::environment_providers::{
    binding_context, environment_view, map_environments_error,
    parse_environment_provider_binding_id, registry_lifecycle_status,
};
use super::*;

use ::environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateEnvironmentEnrollment,
    EnvironmentDaemonEnrollmentRecord, EnvironmentDaemonEnrollmentStore, EnvironmentId,
    EnvironmentIncarnationId, EnvironmentProviderBindingStore, EnvironmentProvisionRequestId,
    EnvironmentSource, EnvironmentStatus, EnvironmentStore, EnvironmentTemplateId,
    FailEnvironmentLifecycle, FinishCloseEnvironment, ListEnvironments,
    ObserveProvisionedEnvironment,
};
use host_protocol::control::targets::{CloseTargetParams, CreateTargetParams, HostTargetStatus};
use rand::RngCore as _;
use sha2::{Digest as _, Sha256};

const DEFAULT_ENROLLMENT_TTL_SECONDS: u32 = 10 * 60;
const MAX_ENROLLMENT_TTL_SECONDS: u32 = 60 * 60;

impl GatewayAgentApi {
    pub(super) async fn create_environment_enrollment_record(
        &self,
        params: EnvironmentEnrollmentCreateParams,
    ) -> Result<EnvironmentEnrollmentCreateResponse, AgentApiError> {
        let request_id =
            EnvironmentProvisionRequestId::try_new(params.request_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid enrollment request id: {error}"))
            })?;
        let ttl_seconds = params
            .expires_in_seconds
            .unwrap_or(DEFAULT_ENROLLMENT_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > MAX_ENROLLMENT_TTL_SECONDS {
            return Err(AgentApiError::invalid_request(format!(
                "expiresInSeconds must be between 1 and {MAX_ENROLLMENT_TTL_SECONDS}"
            )));
        }
        let now = now_ms()?;
        let environment_id = allocate_environment_id();
        let incarnation_id = allocate_incarnation_id();
        let token = mint_enrollment_token(
            self.store.config().universe_id,
            &environment_id,
            &incarnation_id,
        )?;
        let token_hash = Sha256::digest(token.as_bytes()).to_vec();
        let (environment, enrollment, created) =
            EnvironmentDaemonEnrollmentStore::create_enrollment(
                self.store.as_ref(),
                CreateEnvironmentEnrollment {
                    request_id,
                    environment_id,
                    incarnation_id,
                    display_name: params.display_name,
                    metadata: params.metadata,
                    token_hash,
                    token_expires_at_ms: now
                        .checked_add(i64::from(ttl_seconds) * 1_000)
                        .ok_or_else(|| AgentApiError::internal("enrollment expiry overflow"))?,
                    created_at_ms: now,
                },
            )
            .await
            .map_err(map_environments_error)?;
        Ok(EnvironmentEnrollmentCreateResponse {
            environment: environment_view(&environment),
            enrollment: enrollment_view(&enrollment),
            token: created.then_some(token),
        })
    }

    pub(super) async fn read_environment_enrollment_record(
        &self,
        params: EnvironmentEnrollmentReadParams,
    ) -> Result<EnvironmentEnrollmentReadResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let enrollment =
            EnvironmentDaemonEnrollmentStore::read_enrollment(self.store.as_ref(), &environment_id)
                .await
                .map_err(map_environments_error)?;
        Ok(EnvironmentEnrollmentReadResponse {
            enrollment: enrollment_view(&enrollment),
        })
    }

    pub(super) async fn revoke_environment_enrollment_record(
        &self,
        params: EnvironmentEnrollmentRevokeParams,
    ) -> Result<EnvironmentEnrollmentRevokeResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let enrollment = EnvironmentDaemonEnrollmentStore::revoke_enrollment(
            self.store.as_ref(),
            &environment_id,
            now_ms()?,
        )
        .await
        .map_err(map_environments_error)?;
        let route_key = crate::environment_gateway::RouteKey {
            universe_id: self.store.config().universe_id,
            environment_id: enrollment.environment_id.to_string(),
            incarnation_id: enrollment.incarnation_id.to_string(),
        };
        self.environment_routes
            .fence(&crate::environment_gateway::RouteKey {
                universe_id: route_key.universe_id,
                environment_id: route_key.environment_id.clone(),
                incarnation_id: route_key.incarnation_id.clone(),
            })
            .await;
        EnvironmentStore::observe_environment_route(
            self.store.as_ref(),
            ::environments::ObserveEnvironmentRoute {
                environment_id: enrollment.environment_id.clone(),
                incarnation_id: enrollment.incarnation_id.clone(),
                connected: false,
                observed_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentEnrollmentRevokeResponse {
            enrollment: enrollment_view(&enrollment),
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
        let current = EnvironmentStore::read_environment(self.store.as_ref(), &environment_id)
            .await
            .map_err(map_environments_error)?;
        self.environment_routes
            .fence(&crate::environment_gateway::RouteKey {
                universe_id: self.store.config().universe_id,
                environment_id: current.environment_id.to_string(),
                incarnation_id: current.incarnation.incarnation_id.to_string(),
            })
            .await;
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

fn mint_enrollment_token(
    universe_id: uuid::Uuid,
    environment_id: &EnvironmentId,
    incarnation_id: &EnvironmentIncarnationId,
) -> Result<String, AgentApiError> {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    host_protocol::gateway::encode_enrollment_token(
        &universe_id.to_string(),
        environment_id.as_str(),
        incarnation_id.as_str(),
        &bytes,
    )
    .map_err(|error| AgentApiError::internal(format!("encode enrollment token: {error}")))
}

pub(crate) fn enrollment_view(
    record: &EnvironmentDaemonEnrollmentRecord,
) -> EnvironmentEnrollmentView {
    EnvironmentEnrollmentView {
        environment_id: record.environment_id.to_string(),
        incarnation_id: record.incarnation_id.to_string(),
        token_expires_at_ms: record.token_expires_at_ms,
        token_redeemed_at_ms: record.token_redeemed_at_ms,
        revoked_at_ms: record.revoked_at_ms,
        daemon_id: record.daemon_id.as_ref().map(ToString::to_string),
        enrolled_at_ms: record.enrolled_at_ms,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}
