use super::environment_providers::{
    binding_context, environment_view, map_environments_error,
    parse_environment_provider_binding_id, registry_idle_policy, registry_lifecycle_status,
    registry_power_state,
};
use super::*;

use ::environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment, EnvironmentId,
    EnvironmentIncarnationId, EnvironmentProviderBindingStore, EnvironmentProvisionRequestId,
    EnvironmentRecord, EnvironmentSource, EnvironmentStatus, EnvironmentStore,
    EnvironmentTemplateId, FailEnvironmentLifecycle, FinishCloseEnvironment, ListEnvironments,
    ObserveProvisionedEnvironment, PowerState, SetEnvironmentIdlePolicy, SetEnvironmentPower,
};
use environment_protocol::control::ingress::{
    EnsureIngressParams, ProviderIngressStatus, RemoveIngressParams,
};
use environment_protocol::control::targets::{
    AdoptTargetParams, CloseTargetParams, CreateTargetParams, ProviderTargetStatus,
    ProviderTargetSummary, SetTargetPowerParams,
};

/// Map a provider target observation to the logical lifecycle status. A
/// passive provider reports Ready only after its private envd is reachable;
/// no provider presence has to register separately.
pub(crate) fn lifecycle_status_from_target(status: ProviderTargetStatus) -> EnvironmentStatus {
    match status {
        ProviderTargetStatus::Ready => EnvironmentStatus::Ready,
        ProviderTargetStatus::Creating | ProviderTargetStatus::Starting => {
            EnvironmentStatus::Booting
        }
        ProviderTargetStatus::Paused => EnvironmentStatus::Paused,
        ProviderTargetStatus::Suspended => EnvironmentStatus::Suspended,
        ProviderTargetStatus::Stopped => EnvironmentStatus::Offline,
        ProviderTargetStatus::Closing => EnvironmentStatus::Closing,
        ProviderTargetStatus::Closed => EnvironmentStatus::Closed,
        ProviderTargetStatus::Failed => EnvironmentStatus::Failed,
        ProviderTargetStatus::Unknown => EnvironmentStatus::Unknown,
    }
}

impl EnvironmentService {
    pub(crate) async fn put_environment_ingress_record(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<EnvironmentIngressPutResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let environment = EnvironmentStore::read_environment(self.store.as_ref(), &environment_id)
            .await
            .map_err(map_environments_error)?;
        if !matches!(
            environment.status,
            EnvironmentStatus::Ready
                | EnvironmentStatus::Offline
                | EnvironmentStatus::Paused
                | EnvironmentStatus::Suspended
        ) {
            return Err(AgentApiError::rejected(
                "public ingress requires a ready or powered-down environment",
            ));
        }
        let EnvironmentSource::Provisioned {
            provider_id,
            binding_id,
        } = &environment.source
        else {
            return Err(AgentApiError::rejected(
                "provider-managed ingress is unavailable for external environments",
            ));
        };
        let target_id = environment
            .incarnation
            .provider_target_id
            .clone()
            .ok_or_else(|| AgentApiError::rejected("environment has no provider target"))?;
        let provider = self.read_environment_provider(provider_id).await?;
        let binding = EnvironmentProviderBindingStore::read_provider_binding(
            self.store.as_ref(),
            self.store.config().universe_id,
            binding_id,
        )
        .await
        .map_err(map_environments_error)?;
        let mut controller = self
            .provider_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let public_endpoint = async {
            if params.enabled {
                let response = controller
                    .ensure_ingress(&EnsureIngressParams {
                        request_id: format!("ingress-enable:{}", environment.request_id),
                        environment_id: environment.environment_id.to_string(),
                        incarnation_id: environment.incarnation.incarnation_id.to_string(),
                        binding: binding_context(&binding),
                        target_id,
                    })
                    .await?;
                if response.status != ProviderIngressStatus::Ready {
                    return Err(AgentApiError::rejected(
                        "provider did not realize public ingress",
                    ));
                }
                Ok(Some(response.public_endpoint.ok_or_else(|| {
                    AgentApiError::rejected(
                        "provider returned ready ingress without a public endpoint",
                    )
                })?))
            } else {
                let response = controller
                    .remove_ingress(&RemoveIngressParams {
                        request_id: format!("ingress-disable:{}", environment.request_id),
                        environment_id: environment.environment_id.to_string(),
                        incarnation_id: environment.incarnation.incarnation_id.to_string(),
                        binding: binding_context(&binding),
                        target_id,
                    })
                    .await?;
                if response.status != ProviderIngressStatus::Disabled {
                    return Err(AgentApiError::rejected(
                        "provider did not remove public ingress",
                    ));
                }
                Ok(None)
            }
        }
        .await;
        let public_endpoint = finish_provider_controller(controller, public_endpoint).await?;
        let environment = EnvironmentStore::set_environment_ingress(
            self.store.as_ref(),
            ::environments::SetEnvironmentIngress {
                environment_id,
                enabled: params.enabled,
                public_endpoint,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentIngressPutResponse {
            environment: environment_view(&environment),
        })
    }

    pub(crate) async fn create_external_environment_record(
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
                metadata: validated_caller_metadata(params.metadata)?,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentExternalCreateResponse {
            environment: environment_view(&environment),
        })
    }
    pub(crate) async fn create_environment_record(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentCreateResponse, AgentApiError> {
        let environment = self.accept_environment_create(params).await?;
        Ok(EnvironmentCreateResponse {
            environment: environment_view(&environment),
        })
    }

    /// Accept environment creation; provider I/O belongs to the reconciler.
    pub(crate) async fn accept_environment_create(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentRecord, AgentApiError> {
        let request_id =
            EnvironmentProvisionRequestId::try_new(params.request_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid environment request id: {error}"))
            })?;
        let binding_id = parse_environment_provider_binding_id(params.binding_id)?;
        let template_id = EnvironmentTemplateId::try_new(params.template_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid environment template id: {error}"))
        })?;
        let idle_policy = params.idle_policy.as_ref().map(registry_idle_policy);
        if let Some(policy) = &idle_policy {
            policy.validate().map_err(map_environments_error)?;
        }
        EnvironmentStore::create_environment(
            self.store.as_ref(),
            CreateEnvironment {
                request_id,
                environment_id: allocate_environment_id(),
                incarnation_id: allocate_incarnation_id(),
                binding_id,
                template_id,
                display_name: params.display_name,
                metadata: validated_caller_metadata(params.metadata)?,
                idle_policy,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)
    }

    /// `environments/power/put`: record power intent. Provider support is
    /// checked against the states observed on the current incarnation; the
    /// reconciler converges asynchronously.
    pub(crate) async fn put_environment_power_record(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<EnvironmentPowerPutResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let desired_power = registry_power_state(params.power);
        let environment = EnvironmentStore::read_environment(self.store.as_ref(), &environment_id)
            .await
            .map_err(map_environments_error)?;
        if !matches!(environment.source, EnvironmentSource::Provisioned { .. }) {
            return Err(AgentApiError::rejected(
                "external environments have no power control",
            ));
        }
        if desired_power != PowerState::Running
            && !environment
                .incarnation
                .power_states
                .contains(&desired_power)
        {
            return Err(AgentApiError::rejected(
                if environment.incarnation.power_states.is_empty() {
                    format!(
                        "environment power state {desired_power} is unavailable: the provider has not reported power support for this environment yet"
                    )
                } else {
                    format!(
                        "environment power state {desired_power} is not supported by the provider (supported: {})",
                        environment
                            .incarnation
                            .power_states
                            .iter()
                            .map(|state| state.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            ));
        }
        let environment = EnvironmentStore::set_environment_power(
            self.store.as_ref(),
            SetEnvironmentPower {
                environment_id,
                desired_power,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentPowerPutResponse {
            environment: environment_view(&environment),
        })
    }

    /// `environments/idle-policy/put`: replace or clear the staged idle
    /// policy of a provisioned environment.
    pub(crate) async fn put_environment_idle_policy_record(
        &self,
        params: EnvironmentIdlePolicyPutParams,
    ) -> Result<EnvironmentIdlePolicyPutResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let idle_policy = params.idle_policy.as_ref().map(registry_idle_policy);
        if let Some(policy) = &idle_policy {
            policy.validate().map_err(map_environments_error)?;
        }
        let environment = EnvironmentStore::set_environment_idle_policy(
            self.store.as_ref(),
            SetEnvironmentIdlePolicy {
                environment_id,
                idle_policy,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentIdlePolicyPutResponse {
            environment: environment_view(&environment),
        })
    }

    pub(crate) async fn read_environment_record(
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

    pub(crate) async fn list_environment_records(
        &self,
        params: EnvironmentListParams,
    ) -> Result<EnvironmentListResponse, AgentApiError> {
        let environments = EnvironmentStore::list_environments(
            self.store.as_ref(),
            ListEnvironments {
                metadata: params.metadata,
                provider_id: params
                    .provider_id
                    .map(parse_environment_provider_id)
                    .transpose()?,
                binding_id: params
                    .binding_id
                    .map(parse_environment_provider_binding_id)
                    .transpose()?,
                status: params.status.map(registry_lifecycle_status),
                registration_key_id: params
                    .registration_key_id
                    .map(parse_registration_key_id)
                    .transpose()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentListResponse {
            environments: environments.iter().map(environment_view).collect(),
        })
    }

    pub(crate) async fn close_environment_record(
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

    /// Public entry point for one reconciliation pass; used by acceptance
    /// tests that drive the reconciler deterministically instead of running
    /// the background loop.
    pub async fn reconcile_environments_once(&self) -> Result<usize, AgentApiError> {
        self.reconcile_environment_lifecycle_once().await
    }

    /// Reconcile every pending environment in this universe once. Calls are
    /// stable and idempotent; a crash after controller I/O is recovered by a
    /// later pass issuing the same IDs again.
    pub(crate) async fn reconcile_environment_lifecycle_once(
        &self,
    ) -> Result<usize, AgentApiError> {
        let mut changed = self.reconcile_registered_once().await?;
        let environments =
            EnvironmentStore::list_environments_needing_reconcile(self.store.as_ref())
                .await
                .map_err(map_environments_error)?;
        // One unreachable provider must not stall every other environment in
        // the universe: per-environment failures are remembered and the pass
        // continues; the first failure is reported once the pass is complete.
        let mut first_error: Option<AgentApiError> = None;
        for environment in environments {
            let outcome = match environment.status {
                EnvironmentStatus::Closing => self.reconcile_environment_close(&environment).await,
                EnvironmentStatus::Ready
                | EnvironmentStatus::Paused
                | EnvironmentStatus::Suspended
                | EnvironmentStatus::Offline
                    if environment.power_diverges() =>
                {
                    self.reconcile_environment_power(&environment).await
                }
                EnvironmentStatus::Provisioning
                | EnvironmentStatus::Booting
                | EnvironmentStatus::Unknown => {
                    match self.reconcile_environment_create(&environment).await {
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
                            .map_err(map_environments_error)
                            .map(|_| true)
                        }
                        other => other,
                    }
                }
                _ => Ok(false),
            };
            match outcome {
                Ok(did_change) => changed += usize::from(did_change),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(AgentApiError {
                            message: format!(
                                "environment {}: {}",
                                environment.environment_id, error.message
                            ),
                            ..error
                        });
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(changed),
        }
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
        let mut controller = self
            .provider_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let target = async {
            match (
                environment.incarnation.template_id.as_ref(),
                environment.incarnation.adoption_source_target.as_ref(),
            ) {
                (Some(template_id), None) => Ok(controller
                    .create_target(&CreateTargetParams {
                        request_id: environment.request_id.to_string(),
                        environment_id: environment.environment_id.to_string(),
                        incarnation_id: environment.incarnation.incarnation_id.to_string(),
                        binding: binding_context(&binding),
                        template_id: template_id.to_string(),
                    })
                    .await?
                    .target),
                (None, Some(source_target)) => Ok(controller
                    .adopt_target(&AdoptTargetParams {
                        request_id: environment.request_id.to_string(),
                        environment_id: environment.environment_id.to_string(),
                        incarnation_id: environment.incarnation.incarnation_id.to_string(),
                        binding: binding_context(&binding),
                        source_target: source_target.clone(),
                    })
                    .await?
                    .target),
                _ => Err(AgentApiError::internal(
                    "provisioned incarnation has invalid realization",
                )),
            }
        }
        .await;
        let target = finish_provider_controller(controller, target).await?;
        self.record_target_observation(environment, target).await
    }

    /// Persist a provider observation when it changes the record: status,
    /// target id, or the provider-reported power states.
    async fn record_target_observation(
        &self,
        environment: &::environments::EnvironmentRecord,
        target: ProviderTargetSummary,
    ) -> Result<bool, AgentApiError> {
        let status = lifecycle_status_from_target(target.status);
        let changed = environment.status != status
            || environment.incarnation.provider_target_id.as_ref() != Some(&target.target_id)
            || environment.incarnation.power_states != target.power_states;
        if changed {
            EnvironmentStore::observe_provisioned_environment(
                self.store.as_ref(),
                ObserveProvisionedEnvironment {
                    environment_id: environment.environment_id.clone(),
                    provider_target_id: target.target_id,
                    status,
                    power_states: target.power_states,
                    observed_at_ms: now_ms()?,
                },
            )
            .await
            .map_err(map_environments_error)?;
        }
        Ok(changed)
    }

    /// Converge a steady-state provisioned environment toward its desired
    /// power. One idempotent `setTargetPower` per pass; the observed
    /// summary is recorded like any other observation, so a transitional
    /// answer (`Starting`) flows back through the create/observe path.
    async fn reconcile_environment_power(
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
        let Some(target_id) = environment.incarnation.provider_target_id.clone() else {
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
            .provider_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = controller
            .set_target_power(&SetTargetPowerParams {
                request_id: format!(
                    "power:{}:{}",
                    environment.request_id, environment.desired_power
                ),
                environment_id: environment.environment_id.to_string(),
                incarnation_id: environment.incarnation.incarnation_id.to_string(),
                binding: binding_context(&binding),
                target_id,
                power: environment.desired_power,
            })
            .await;
        let response = finish_provider_controller(controller, response).await?;
        self.record_target_observation(environment, response.target)
            .await
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
            .provider_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = async {
            let ingress_response = controller
                .remove_ingress(&RemoveIngressParams {
                    request_id: format!("ingress-close:{}", environment.request_id),
                    environment_id: environment.environment_id.to_string(),
                    incarnation_id: environment.incarnation.incarnation_id.to_string(),
                    binding: binding_context(&binding),
                    target_id: target_id.clone(),
                })
                .await?;
            if ingress_response.status != ProviderIngressStatus::Disabled {
                return Err(AgentApiError::rejected(
                    "provider did not remove ingress before target close",
                ));
            }
            controller
                .close_target(&CloseTargetParams {
                    request_id: format!("close:{}", environment.request_id),
                    environment_id: environment.environment_id.to_string(),
                    incarnation_id: environment.incarnation.incarnation_id.to_string(),
                    binding: binding_context(&binding),
                    target_id,
                    force: false,
                })
                .await
        }
        .await;
        let response = finish_provider_controller(controller, response).await?;
        if response.status == ProviderTargetStatus::Closed {
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
            environment_protocol::shared::EnvironmentTransport::WebSocket
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

/// Rate-limits repeated reconcile-pass failures per universe so a provider
/// that stays unreachable produces one warning per minute (or one per
/// distinct message) instead of one every tick.
#[derive(Default)]
pub struct ReconcileFailureLog {
    last: std::collections::HashMap<uuid::Uuid, (String, std::time::Instant)>,
}

impl ReconcileFailureLog {
    const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

    /// Record a failed pass; log at `warn` when it is new or stale, else at
    /// `debug`.
    pub fn failed(&mut self, universe_id: uuid::Uuid, error: &AgentApiError) {
        let message = error.to_string();
        let now = std::time::Instant::now();
        let repeat = self.last.get(&universe_id).is_some_and(|(last, at)| {
            *last == message && now.duration_since(*at) < Self::REPEAT_INTERVAL
        });
        if repeat {
            tracing::debug!(target: "temporal_server", %universe_id, error = %message, "environment lifecycle reconcile pass still failing");
            return;
        }
        self.last.insert(universe_id, (message.clone(), now));
        tracing::warn!(target: "temporal_server", %universe_id, error = %message, "environment lifecycle reconcile pass failed (repeats suppressed for 60s)");
    }

    /// Record a successful pass so the next failure logs immediately.
    pub fn succeeded(&mut self, universe_id: uuid::Uuid) {
        self.last.remove(&universe_id);
    }
}

pub(crate) fn parse_registry_environment_id(value: String) -> Result<EnvironmentId, AgentApiError> {
    EnvironmentId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid environment id: {error}")))
}

pub(crate) fn parse_registration_key_id(
    value: String,
) -> Result<::environments::EnvironmentRegistrationKeyId, AgentApiError> {
    ::environments::EnvironmentRegistrationKeyId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid registration key id: {error}"))
    })
}

pub(crate) fn allocate_environment_id() -> EnvironmentId {
    EnvironmentId::new(format!("environment_{}", uuid::Uuid::new_v4().simple()))
}

pub(crate) fn allocate_incarnation_id() -> EnvironmentIncarnationId {
    EnvironmentIncarnationId::new(format!("incarnation_{}", uuid::Uuid::new_v4().simple()))
}

/// Caller-supplied environment metadata: the same bounds and reserved-prefix
/// rule as session metadata, checked before the record is written.
fn validated_caller_metadata(
    metadata: std::collections::BTreeMap<String, String>,
) -> Result<std::collections::BTreeMap<String, String>, AgentApiError> {
    super::validate_caller_metadata(&metadata)?;
    Ok(metadata)
}
