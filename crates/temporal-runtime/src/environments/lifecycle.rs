use super::provider_controllers::{
    ControllerDisposition, ControllerError, ControllerFeature, ProviderController, ProviderPass,
    provider_connect_options,
};
use super::providers::{
    environment_view, map_environments_error, parse_environment_provider_binding_id,
    registry_idle_policy, registry_lifecycle_status, registry_power_state,
};
use super::*;

use ::environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment, DaemonBuild,
    EnvironmentEndpoint, EnvironmentId, EnvironmentIncarnationId, EnvironmentLifecycleError,
    EnvironmentProviderId, EnvironmentProvisionRequestId, EnvironmentRecord,
    EnvironmentRegistryError, EnvironmentSource, EnvironmentStatus, EnvironmentStore,
    EnvironmentTemplateId, FailEnvironmentLifecycle, FinishCloseEnvironment, ListEnvironments,
    ObserveProvisionedEnvironment, PowerState, ReconcileScope, RecordEnvironmentIngress,
    RecordEnvironmentLifecycleError, SetEnvironmentIdlePolicy, SetEnvironmentIngress,
    SetEnvironmentPower,
};
use auth::SecretValue;
use environment_client::{ClientError, DataClient};
use environment_protocol::{
    control::{
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            AdoptTargetParams, CloseTargetParams, CreateTargetParams, GetTargetParams,
            SetTargetPowerParams, TargetObservation, TargetOwner, TargetPhase, TargetRef,
            TargetState,
        },
    },
    error::PreconditionReason,
    ids::{BindingId, IncarnationId, ProviderNativeRef, TemplateId, UniverseId},
    routes::provider_data_route,
};

/// How long a provisioning, booting or unknown environment may go without
/// converging (transient errors such as an unreachable provider, or
/// observations that make no progress, such as a guest whose envd never
/// answers) before it is failed with the last error.
pub(crate) const PROVISIONING_DEADLINE_MS: i64 = 30 * 60 * 1000;

/// How often a reconcile pass also observes steady provisioned
/// environments with `target/get`, so a target deleted or changed out of
/// band is noticed and ingress converges.
pub(crate) const STEADY_OBSERVATION_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(120);

/// A repeated transient error is written again only after this long, so a
/// provider that stays down does not rewrite every row on every pass.
const LIFECYCLE_ERROR_REFRESH_MS: i64 = 30 * 1000;

/// What one target observation means for its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetVerdict {
    /// Record this status.
    Status(EnvironmentStatus),
    /// Running, and the provider's own probe reached envd: Ready once the
    /// runtime's data-plane handshake succeeds.
    DaemonReady,
    /// Broken or gone for good.
    Failed(String),
    /// The provider cannot tell; retried like a transient error.
    Unknown(String),
}

/// Map a target's two-axis state to the environment lifecycle. A target
/// that closes while its environment is not closing was closed out of band.
pub(crate) fn target_verdict(state: &TargetState) -> TargetVerdict {
    let message = state.message.as_ref().map(|message| message.as_str());
    match state.phase {
        TargetPhase::Provisioning => TargetVerdict::Status(EnvironmentStatus::Provisioning),
        TargetPhase::Active => match state.power {
            Some(PowerState::Running) if state.daemon_ready => TargetVerdict::DaemonReady,
            Some(PowerState::Running) => TargetVerdict::Status(EnvironmentStatus::Booting),
            Some(PowerState::Paused) => TargetVerdict::Status(EnvironmentStatus::Paused),
            Some(PowerState::Suspended) => TargetVerdict::Status(EnvironmentStatus::Suspended),
            Some(PowerState::Stopped) => TargetVerdict::Status(EnvironmentStatus::Offline),
            None => TargetVerdict::Unknown(
                "the provider reported an active target without a power state".to_owned(),
            ),
        },
        TargetPhase::Closing => {
            TargetVerdict::Failed("the provider is closing the environment's target".to_owned())
        }
        TargetPhase::Closed => {
            TargetVerdict::Failed("the provider closed the environment's target".to_owned())
        }
        TargetPhase::Failed => TargetVerdict::Failed(
            message
                .unwrap_or("the provider reported the target failed")
                .to_owned(),
        ),
        TargetPhase::Unknown => TargetVerdict::Unknown(
            message
                .unwrap_or("the provider cannot tell the target's state")
                .to_owned(),
        ),
    }
}

/// The lifecycle note for an observation that leaves a converging
/// environment where it was: recorded like a transient error, so the
/// provisioning deadline runs while nothing progresses.
pub(crate) fn waiting_note(status: EnvironmentStatus, state: &TargetState) -> String {
    let waiting = match status {
        EnvironmentStatus::Booting => "waiting for envd to answer on the target",
        _ => "waiting for the provider to provision the target",
    };
    match &state.message {
        Some(message) => format!("{waiting}: {}", message.as_str()),
        None => waiting.to_owned(),
    }
}

/// True once transient errors or stalled observations have persisted past
/// the provisioning deadline.
pub(crate) fn provisioning_deadline_passed(
    error: Option<&EnvironmentLifecycleError>,
    now_ms: i64,
) -> bool {
    error.is_some_and(|error| now_ms.saturating_sub(error.first_at_ms) >= PROVISIONING_DEADLINE_MS)
}

/// True when `message` at `now_ms` is worth a write over `recorded`.
fn lifecycle_error_is_news(
    recorded: Option<&EnvironmentLifecycleError>,
    message: &str,
    now_ms: i64,
) -> bool {
    let cleaned = EnvironmentLifecycleError::record(None, message, now_ms).message;
    recorded.is_none_or(|recorded| {
        recorded.message != cleaned
            || now_ms.saturating_sub(recorded.last_at_ms) >= LIFECYCLE_ERROR_REFRESH_MS
    })
}

/// The outcome of the data-plane handshake at the Ready transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DaemonProbe {
    Ready(DaemonBuild),
    /// envd speaks another protocol version and can never serve this
    /// runtime.
    Mismatch {
        local: u32,
        remote: u32,
    },
    /// envd did not complete the handshake; retried.
    Unreachable(String),
}

/// Run `initialize` against a target's data route, with the provider
/// credential, and close the connection again.
pub(crate) async fn probe_daemon(endpoint: &str, credential: &SecretValue) -> DaemonProbe {
    match DataClient::connect_initialized(endpoint, provider_connect_options(credential)).await {
        Ok((client, initialized)) => {
            let _ = client.close().await;
            DaemonProbe::Ready(DaemonBuild::from_implementation(
                &initialized.server_info,
                initialized.protocol_version,
            ))
        }
        Err(ClientError::VersionMismatch { local, remote, .. }) => {
            DaemonProbe::Mismatch { local, remote }
        }
        Err(error) => DaemonProbe::Unreachable(format!(
            "envd did not complete the data-plane handshake: {error}"
        )),
    }
}

/// The controller-plane owner of an environment's current incarnation.
pub(crate) fn target_owner(
    universe_id: uuid::Uuid,
    environment: &EnvironmentRecord,
) -> Result<TargetOwner, ControllerError> {
    let binding_id = environment
        .binding_id()
        .ok_or_else(|| invalid_registry("only provisioned environments have a provider target"))?;
    let invalid = |error: environment_protocol::ids::IdError| {
        invalid_registry(format!(
            "environment ids do not fit the controller protocol: {error}"
        ))
    };
    Ok(TargetOwner {
        universe_id: UniverseId::new(universe_id.to_string()).map_err(invalid)?,
        binding_id: BindingId::new(binding_id.as_str()).map_err(invalid)?,
        environment_id: environment_protocol::ids::EnvironmentId::new(
            environment.environment_id.as_str(),
        )
        .map_err(invalid)?,
        incarnation_id: IncarnationId::new(environment.incarnation.incarnation_id.as_str())
            .map_err(invalid)?,
    })
}

/// Wall-clock milliseconds for reconciler writes.
fn clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn invalid_registry(message: impl Into<String>) -> ControllerError {
    ControllerError::Registry(EnvironmentRegistryError::InvalidInput {
        message: message.into(),
    })
}

impl EnvironmentService {
    /// `environments/ingress/put`: record the public ingress intent. The
    /// reconciler converges the provider toward it while the environment is
    /// ready and records the realized endpoint. Enabling is refused up front
    /// when the provider does not offer ingress.
    pub(crate) async fn put_environment_ingress_record(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<EnvironmentIngressPutResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let environment = EnvironmentStore::read_environment(self.store.as_ref(), &environment_id)
            .await
            .map_err(map_environments_error)?;
        if matches!(
            environment.status,
            EnvironmentStatus::Closing | EnvironmentStatus::Closed | EnvironmentStatus::Failed
        ) {
            return Err(AgentApiError::rejected(
                "public ingress requires an environment that is not closing, closed or failed",
            ));
        }
        let EnvironmentSource::Provisioned { provider_id, .. } = &environment.source else {
            return Err(AgentApiError::rejected(
                "provider-managed ingress is unavailable for external environments",
            ));
        };
        if params.enabled {
            self.require_controller_feature(provider_id, ControllerFeature::Ingress)
                .await?;
        }
        let environment = EnvironmentStore::set_environment_ingress(
            self.store.as_ref(),
            SetEnvironmentIngress {
                environment_id,
                enabled: params.enabled,
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
        let endpoint = EnvironmentEndpoint::parse(&params.endpoint).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid environment endpoint: {error}"))
        })?;
        let environment = EnvironmentStore::create_external_environment(
            self.store.as_ref(),
            CreateExternalEnvironment {
                request_id,
                environment_id: allocate_environment_id(),
                incarnation_id: allocate_incarnation_id(),
                endpoint,
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
                    .map(super::providers::parse_environment_provider_id)
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
    /// The pass includes steady environments.
    pub async fn reconcile_environments_once(&self) -> Result<usize, AgentApiError> {
        let mut pass = ProviderPass::default();
        let result = self
            .reconcile_environment_lifecycle_pass(&mut pass, ReconcileScope::WithSteady)
            .await;
        pass.close().await;
        result
    }

    /// Reconcile this universe once, sharing `pass`'s controller
    /// connections and unhealthy-provider skips with the rest of the
    /// deployment pass. The registered sweep and the provisioned pass run
    /// independently: an error in one never stops the other, and one
    /// environment's error never stops the rest.
    pub(crate) async fn reconcile_environment_lifecycle_pass(
        &self,
        pass: &mut ProviderPass,
        scope: ReconcileScope,
    ) -> Result<usize, AgentApiError> {
        let registered = self.reconcile_registered_once().await;
        let provisioned = self.reconcile_provisioned_once(pass, scope).await;
        match (registered, provisioned) {
            (Ok(registered), Ok(provisioned)) => Ok(registered + provisioned),
            (Err(error), Ok(_)) => Err(AgentApiError {
                message: format!("registered environments: {}", error.message),
                ..error
            }),
            (Ok(_), Err(error)) => Err(error),
            (Err(registered), Err(provisioned)) => Err(AgentApiError {
                message: format!(
                    "registered environments: {}; {}",
                    registered.message, provisioned.message
                ),
                ..provisioned
            }),
        }
    }

    /// One pass over every environment `scope` selects. Calls are
    /// idempotent per owner, so a crash after controller I/O is recovered
    /// by a later pass making the same calls again.
    async fn reconcile_provisioned_once(
        &self,
        pass: &mut ProviderPass,
        scope: ReconcileScope,
    ) -> Result<usize, AgentApiError> {
        let environments =
            EnvironmentStore::list_environments_needing_reconcile(self.store.as_ref(), scope)
                .await
                .map_err(map_environments_error)?;
        let mut changed = 0;
        let mut first_error: Option<AgentApiError> = None;
        for environment in environments {
            match self.reconcile_environment(pass, &environment).await {
                Ok(did_change) => changed += usize::from(did_change),
                Err(error) => {
                    tracing::debug!(
                        target: "temporal_runtime",
                        environment_id = %environment.environment_id,
                        %error,
                        "environment reconcile step failed"
                    );
                    first_error.get_or_insert(AgentApiError {
                        message: format!(
                            "environment {}: {}",
                            environment.environment_id, error.message
                        ),
                        ..error
                    });
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(changed),
        }
    }

    async fn reconcile_environment(
        &self,
        pass: &mut ProviderPass,
        environment: &EnvironmentRecord,
    ) -> Result<bool, AgentApiError> {
        let EnvironmentSource::Provisioned { provider_id, .. } = &environment.source else {
            // External and registered environments hold no provider target.
            if environment.status == EnvironmentStatus::Closing {
                self.finish_close(environment)
                    .await
                    .map_err(map_environments_error)?;
                return Ok(true);
            }
            return Ok(false);
        };
        let step = match environment.status {
            EnvironmentStatus::Closing => self.close_target(pass, environment, provider_id).await,
            EnvironmentStatus::Provisioning
            | EnvironmentStatus::Booting
            | EnvironmentStatus::Unknown => {
                self.observe_target(pass, environment, provider_id).await
            }
            _ if environment.power_diverges() => {
                self.converge_power(pass, environment, provider_id).await
            }
            // Steady targets (and ready ones whose ingress diverges) are
            // observed with `target/get`: a vanished target fails the
            // environment, and ingress converges from the observation.
            _ if environment.is_steady_provisioned() => {
                self.observe_target(pass, environment, provider_id).await
            }
            _ => return Ok(false),
        };
        match step {
            Ok(changed) => Ok(changed),
            Err(error) => {
                pass.observe_failure(&error).await;
                self.controller_failure(environment, error).await
            }
        }
    }

    /// Decide what a failed controller interaction does to the environment.
    async fn controller_failure(
        &self,
        environment: &EnvironmentRecord,
        error: ControllerError,
    ) -> Result<bool, AgentApiError> {
        if let ControllerError::Registry(error) = error {
            return Err(map_environments_error(error));
        }
        let converging = matches!(
            environment.status,
            EnvironmentStatus::Closing
                | EnvironmentStatus::Provisioning
                | EnvironmentStatus::Booting
                | EnvironmentStatus::Unknown
        );
        if !converging && error.disposition() != ControllerDisposition::Gone {
            // A steady environment whose power change failed keeps serving
            // in its current state. The error is reported with the pass
            // instead of recorded, so it cannot start a provisioning
            // deadline that a later wake would inherit.
            return Err(error.into_api_error());
        }
        self.decide_controller_failure(environment, error)
            .await
            .map_err(map_environments_error)
    }

    async fn decide_controller_failure(
        &self,
        environment: &EnvironmentRecord,
        error: ControllerError,
    ) -> Result<bool, EnvironmentRegistryError> {
        let error = match error {
            ControllerError::Registry(error) => return Err(error),
            error => error,
        };
        let message = error.to_string();
        let disposition = error.disposition();
        match environment.status {
            EnvironmentStatus::Closing if disposition == ControllerDisposition::Gone => {
                self.finish_close(environment).await?;
                Ok(true)
            }
            EnvironmentStatus::Closing => self.record_lifecycle_error(environment, &message).await,
            EnvironmentStatus::Provisioning
            | EnvironmentStatus::Booting
            | EnvironmentStatus::Unknown => match disposition {
                ControllerDisposition::Transient => {
                    self.transient_failure(environment, &message).await
                }
                ControllerDisposition::Refused => self.fail(environment, &message, None).await,
                ControllerDisposition::Gone => {
                    let message = if environment.incarnation.provider_target_id.is_some() {
                        format!("provider target disappeared: {message}")
                    } else {
                        message
                    };
                    self.fail(environment, &message, None).await
                }
            },
            // Only a vanished target reaches here for a steady environment.
            _ => {
                self.fail(
                    environment,
                    &format!("provider target disappeared: {message}"),
                    None,
                )
                .await
            }
        }
    }

    /// Keep the status and record the error; fail once errors persisted
    /// past the provisioning deadline.
    async fn transient_failure(
        &self,
        environment: &EnvironmentRecord,
        message: &str,
    ) -> Result<bool, EnvironmentRegistryError> {
        let now = clock_ms();
        if provisioning_deadline_passed(environment.lifecycle_error.as_ref(), now) {
            return self
                .fail(
                    environment,
                    &format!(
                        "provisioning did not finish within {} minutes; last error: {message}",
                        PROVISIONING_DEADLINE_MS / 60_000
                    ),
                    None,
                )
                .await;
        }
        self.record_lifecycle_error(environment, message).await
    }

    async fn record_lifecycle_error(
        &self,
        environment: &EnvironmentRecord,
        message: &str,
    ) -> Result<bool, EnvironmentRegistryError> {
        let now = clock_ms();
        if !lifecycle_error_is_news(environment.lifecycle_error.as_ref(), message, now) {
            return Ok(false);
        }
        EnvironmentStore::record_environment_lifecycle_error(
            self.store.as_ref(),
            RecordEnvironmentLifecycleError {
                environment_id: environment.environment_id.clone(),
                message: message.to_owned(),
                observed_at_ms: now,
            },
        )
        .await?;
        Ok(true)
    }

    async fn fail(
        &self,
        environment: &EnvironmentRecord,
        message: &str,
        daemon_build: Option<DaemonBuild>,
    ) -> Result<bool, EnvironmentRegistryError> {
        EnvironmentStore::fail_environment_lifecycle(
            self.store.as_ref(),
            FailEnvironmentLifecycle {
                environment_id: environment.environment_id.clone(),
                message: message.to_owned(),
                daemon_build,
                observed_at_ms: clock_ms(),
            },
        )
        .await?;
        Ok(true)
    }

    async fn finish_close(
        &self,
        environment: &EnvironmentRecord,
    ) -> Result<(), EnvironmentRegistryError> {
        EnvironmentStore::finish_close_environment(
            self.store.as_ref(),
            FinishCloseEnvironment {
                environment_id: environment.environment_id.clone(),
                observed_at_ms: clock_ms(),
            },
        )
        .await?;
        Ok(())
    }

    /// Converging and steady environments: create (or adopt) while no
    /// target id is recorded, observe with `target/get` once one is.
    /// Creation is never re-sent for a known target.
    async fn observe_target(
        &self,
        pass: &mut ProviderPass,
        environment: &EnvironmentRecord,
        provider_id: &EnvironmentProviderId,
    ) -> Result<bool, ControllerError> {
        let controller = pass
            .controller(
                self.store.as_ref(),
                self.provider_controller_connector.as_ref(),
                provider_id,
            )
            .await?;
        let owner = target_owner(self.store.config().universe_id, environment)?;
        let response = match (
            &environment.incarnation.provider_target_id,
            &environment.incarnation.template_id,
            &environment.incarnation.adoption_source_target,
        ) {
            (Some(target_id), _, _) => {
                controller
                    .client
                    .get_target(&GetTargetParams {
                        target: TargetRef {
                            owner: owner.clone(),
                            target_id: Some(target_id.clone()),
                        },
                    })
                    .await
            }
            (None, Some(template_id), None) => {
                let template_id = TemplateId::new(template_id.as_str()).map_err(|error| {
                    invalid_registry(format!("template id does not fit the protocol: {error}"))
                })?;
                controller
                    .client
                    .create_target(&CreateTargetParams {
                        owner: owner.clone(),
                        template_id,
                    })
                    .await
            }
            (None, None, Some(source_target)) => {
                controller.require(ControllerFeature::AdoptTarget)?;
                let source_target =
                    ProviderNativeRef::new(source_target.as_str()).map_err(|error| {
                        invalid_registry(format!("adoption source is not valid: {error}"))
                    })?;
                controller
                    .client
                    .adopt_target(&AdoptTargetParams {
                        owner: owner.clone(),
                        source_target,
                    })
                    .await
            }
            _ => {
                return Err(invalid_registry(
                    "provisioned incarnation has neither a template nor an adoption source",
                ));
            }
        };
        let observation = response
            .map_err(|error| controller.call_error(error))?
            .target;
        self.apply_observation(&controller, environment, &owner, observation)
            .await
    }

    /// Steady environments whose power diverges from the intent: one
    /// idempotent `target/setPower` per pass. A target that is not active
    /// is observed instead.
    async fn converge_power(
        &self,
        pass: &mut ProviderPass,
        environment: &EnvironmentRecord,
        provider_id: &EnvironmentProviderId,
    ) -> Result<bool, ControllerError> {
        let Some(target_id) = environment.incarnation.provider_target_id.clone() else {
            return self.observe_target(pass, environment, provider_id).await;
        };
        let controller = pass
            .controller(
                self.store.as_ref(),
                self.provider_controller_connector.as_ref(),
                provider_id,
            )
            .await?;
        let owner = target_owner(self.store.config().universe_id, environment)?;
        let target = TargetRef {
            owner: owner.clone(),
            target_id: Some(target_id),
        };
        let observation = match controller
            .client
            .set_target_power(&SetTargetPowerParams {
                target: target.clone(),
                power: environment.desired_power,
            })
            .await
        {
            Ok(response) => response.target,
            Err(error) => {
                let error = controller.call_error(error);
                if error.precondition() != Some(PreconditionReason::TargetNotActive) {
                    return Err(error);
                }
                controller
                    .client
                    .get_target(&GetTargetParams { target })
                    .await
                    .map_err(|error| controller.call_error(error))?
                    .target
            }
        };
        self.apply_observation(&controller, environment, &owner, observation)
            .await
    }

    /// Close by owner, with the target id when one is recorded: the provider
    /// destroys whatever the owner holds, ingress included, and answers
    /// `closed` once nothing is left. A closing target is closed again on
    /// later passes (close is idempotent).
    async fn close_target(
        &self,
        pass: &mut ProviderPass,
        environment: &EnvironmentRecord,
        provider_id: &EnvironmentProviderId,
    ) -> Result<bool, ControllerError> {
        let controller = match pass
            .controller(
                self.store.as_ref(),
                self.provider_controller_connector.as_ref(),
                provider_id,
            )
            .await
        {
            Err(ControllerError::Registry(EnvironmentRegistryError::NotFound { .. })) => {
                // The provider record is gone, so nothing can reach its
                // targets any more.
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %environment.environment_id,
                    %provider_id,
                    "closing an environment whose provider is no longer registered"
                );
                self.finish_close(environment).await?;
                return Ok(true);
            }
            other => other?,
        };
        let owner = target_owner(self.store.config().universe_id, environment)?;
        let observation = controller
            .client
            .close_target(&CloseTargetParams {
                target: TargetRef {
                    owner,
                    target_id: environment.incarnation.provider_target_id.clone(),
                },
            })
            .await
            .map_err(|error| controller.call_error(error))?
            .target;
        if observation.state.phase == TargetPhase::Closed {
            self.finish_close(environment).await?;
            return Ok(true);
        }
        let (_, changed) = self
            .record_observation(environment, &observation, environment.status, None)
            .await?;
        Ok(changed)
    }

    /// Record an observation and act on it: the Ready transition runs the
    /// data-plane handshake first, and ingress converges against the
    /// recorded intent on running targets.
    async fn apply_observation(
        &self,
        controller: &ProviderController,
        environment: &EnvironmentRecord,
        owner: &TargetOwner,
        observation: TargetObservation,
    ) -> Result<bool, ControllerError> {
        if observation.owner != *owner {
            return Err(controller.call_error(ClientError::Decode(
                "the provider answered with another owner's target".to_owned(),
            )));
        }
        match target_verdict(&observation.state) {
            // A ready environment became ready through the runtime's own
            // data-plane handshake; the provider's liveness probe alone does
            // not demote it.
            TargetVerdict::Status(EnvironmentStatus::Booting)
                if environment.status == EnvironmentStatus::Ready =>
            {
                let (recorded, changed) = self
                    .record_observation(environment, &observation, EnvironmentStatus::Ready, None)
                    .await?;
                let converged = self
                    .converge_ingress(controller, &recorded, owner, &observation)
                    .await;
                Ok(changed || converged)
            }
            TargetVerdict::Status(status) => {
                let (recorded, changed) = self
                    .record_observation(environment, &observation, status, None)
                    .await?;
                let stalled = environment.status == status
                    && matches!(
                        status,
                        EnvironmentStatus::Provisioning | EnvironmentStatus::Booting
                    );
                if !stalled {
                    return Ok(changed);
                }
                let noted = self
                    .transient_failure(&recorded, &waiting_note(status, &observation.state))
                    .await?;
                Ok(changed || noted)
            }
            TargetVerdict::Failed(message) => {
                self.record_observation(environment, &observation, environment.status, None)
                    .await?;
                Ok(self.fail(environment, &message, None).await?)
            }
            TargetVerdict::Unknown(message) => {
                let (recorded, _) = self
                    .record_observation(environment, &observation, EnvironmentStatus::Unknown, None)
                    .await?;
                self.transient_failure(&recorded, &message).await?;
                Ok(true)
            }
            TargetVerdict::DaemonReady if environment.status == EnvironmentStatus::Ready => {
                let (recorded, changed) = self
                    .record_observation(environment, &observation, EnvironmentStatus::Ready, None)
                    .await?;
                let converged = self
                    .converge_ingress(controller, &recorded, owner, &observation)
                    .await;
                Ok(changed || converged)
            }
            TargetVerdict::DaemonReady => {
                let Some(target_id) = &observation.target_id else {
                    let (recorded, _) = self
                        .record_observation(
                            environment,
                            &observation,
                            EnvironmentStatus::Booting,
                            None,
                        )
                        .await?;
                    self.transient_failure(
                        &recorded,
                        "the provider reported a running target without a targetId",
                    )
                    .await?;
                    return Ok(true);
                };
                let endpoint = provider_data_route(&controller.data_route_base, owner, target_id);
                match probe_daemon(&endpoint, &controller.credential).await {
                    DaemonProbe::Ready(build) => {
                        let (recorded, _) = self
                            .record_observation(
                                environment,
                                &observation,
                                EnvironmentStatus::Ready,
                                Some(build),
                            )
                            .await?;
                        self.converge_ingress(controller, &recorded, owner, &observation)
                            .await;
                        Ok(true)
                    }
                    DaemonProbe::Mismatch { local, remote } => {
                        self.record_observation(
                            environment,
                            &observation,
                            environment.status,
                            None,
                        )
                        .await?;
                        self.fail(
                            environment,
                            &format!(
                                "the environment's envd speaks protocol version {remote}; this runtime requires {local}"
                            ),
                            Some(DaemonBuild {
                                version: None,
                                git_sha: None,
                                protocol_version: remote,
                            }),
                        )
                        .await
                        .map_err(ControllerError::from)
                    }
                    DaemonProbe::Unreachable(message) => {
                        let (recorded, _) = self
                            .record_observation(
                                environment,
                                &observation,
                                EnvironmentStatus::Booting,
                                None,
                            )
                            .await?;
                        self.transient_failure(&recorded, &message).await?;
                        Ok(true)
                    }
                }
            }
        }
    }

    /// Persist an observation when it changes the record: status, target
    /// id, power states or the daemon build. Returns the current record.
    async fn record_observation(
        &self,
        environment: &EnvironmentRecord,
        observation: &TargetObservation,
        status: EnvironmentStatus,
        daemon_build: Option<DaemonBuild>,
    ) -> Result<(EnvironmentRecord, bool), ControllerError> {
        let target_changed = observation.target_id.is_some()
            && environment.incarnation.provider_target_id != observation.target_id;
        let changed = environment.status != status
            || target_changed
            || environment.incarnation.power_states != observation.supported_power_states
            || daemon_build.is_some();
        if !changed {
            return Ok((environment.clone(), false));
        }
        let recorded = EnvironmentStore::observe_provisioned_environment(
            self.store.as_ref(),
            ObserveProvisionedEnvironment {
                environment_id: environment.environment_id.clone(),
                provider_target_id: observation.target_id.clone(),
                status,
                power_states: observation.supported_power_states.clone(),
                daemon_build,
                observed_at_ms: clock_ms(),
            },
        )
        .await?;
        Ok((recorded, true))
    }

    /// Converge a running target's ingress toward the recorded intent. The
    /// recorded endpoint always mirrors what the provider last reported.
    /// Ingress methods answer promptly and may continue in the background,
    /// so an answer that has not converged yet is pending: the intent stays
    /// and a later observation checks again. Only a typed refusal of
    /// `ingress/ensure` drops the intent. Ingress failures never fail the
    /// environment and are never recorded as lifecycle errors, so they
    /// cannot start a provisioning deadline. Returns whether the record
    /// changed.
    async fn converge_ingress(
        &self,
        controller: &ProviderController,
        environment: &EnvironmentRecord,
        owner: &TargetOwner,
        observation: &TargetObservation,
    ) -> bool {
        let realized = realized_ingress(observation);
        let mut changed = self
            .record_realized_ingress(
                environment,
                environment.public_endpoint.as_deref(),
                &realized,
            )
            .await;
        let mut recorded = realized;
        if environment.public_ingress_enabled == recorded.is_some() {
            return changed;
        }
        let enabling = environment.public_ingress_enabled;
        if !controller.features.ingress {
            if enabling {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %environment.environment_id,
                    provider_id = %controller.provider_id,
                    "the provider no longer offers public ingress; the ingress intent is dropped"
                );
                changed |= self.drop_ingress_intent(environment).await;
            }
            return changed;
        }
        let target = TargetRef {
            owner: owner.clone(),
            target_id: observation.target_id.clone(),
        };
        let answer = if enabling {
            controller
                .client
                .ensure_ingress(&EnsureIngressParams { target })
                .await
        } else {
            controller
                .client
                .remove_ingress(&RemoveIngressParams { target })
                .await
        };
        match answer.map_err(|error| controller.call_error(error)) {
            Ok(response) => {
                let answered = realized_ingress(&response.target);
                changed |= self
                    .record_realized_ingress(environment, recorded.as_deref(), &answered)
                    .await;
                recorded = answered;
                if environment.public_ingress_enabled != recorded.is_some() {
                    tracing::debug!(
                        target: "temporal_runtime",
                        environment_id = %environment.environment_id,
                        enabling,
                        "public ingress change is still in progress at the provider"
                    );
                }
            }
            Err(error) if enabling && error.disposition() == ControllerDisposition::Refused => {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %environment.environment_id,
                    %error,
                    "the provider refused public ingress; the ingress intent is dropped"
                );
                changed |= self.drop_ingress_intent(environment).await;
            }
            Err(error) => {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %environment.environment_id,
                    %error,
                    "{}; retried at the next observation",
                    if enabling {
                        "enabling public ingress failed"
                    } else {
                        "removing public ingress failed"
                    }
                );
            }
        }
        changed
    }

    /// Record `realized` as the environment's public endpoint when it
    /// differs from `recorded`.
    async fn record_realized_ingress(
        &self,
        environment: &EnvironmentRecord,
        recorded: Option<&str>,
        realized: &Option<String>,
    ) -> bool {
        if recorded == realized.as_deref() {
            return false;
        }
        let written = EnvironmentStore::record_environment_ingress(
            self.store.as_ref(),
            RecordEnvironmentIngress {
                environment_id: environment.environment_id.clone(),
                public_endpoint: realized.clone(),
                observed_at_ms: clock_ms(),
            },
        )
        .await;
        match written {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %environment.environment_id,
                    %error,
                    "recording the realized public ingress failed"
                );
                false
            }
        }
    }

    /// Clear an ingress intent the provider cannot realize.
    async fn drop_ingress_intent(&self, environment: &EnvironmentRecord) -> bool {
        let written = EnvironmentStore::set_environment_ingress(
            self.store.as_ref(),
            SetEnvironmentIngress {
                environment_id: environment.environment_id.clone(),
                enabled: false,
                updated_at_ms: clock_ms(),
            },
        )
        .await;
        match written {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %environment.environment_id,
                    %error,
                    "dropping the public ingress intent failed"
                );
                false
            }
        }
    }
}

/// The public endpoint an observation reports realized.
fn realized_ingress(observation: &TargetObservation) -> Option<String> {
    observation
        .ingress
        .as_ref()
        .map(|ingress| ingress.endpoint.as_str().to_owned())
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
            tracing::debug!(target: "temporal_runtime", %universe_id, error = %message, "environment lifecycle reconcile pass still failing");
            return;
        }
        self.last.insert(universe_id, (message.clone(), now));
        tracing::warn!(target: "temporal_runtime", %universe_id, error = %message, "environment lifecycle reconcile pass failed (repeats suppressed for 60s)");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environments::fake_controller::{FakeController, FakeControllerOptions};
    use environment_protocol::{
        ENVD_PROTOCOL_VERSION, control::target::TargetRef as Ref, metadata::DescriptiveText,
        routes::DataRouteBase,
    };

    fn state(
        phase: TargetPhase,
        power: Option<PowerState>,
        daemon_ready: bool,
        message: Option<&str>,
    ) -> TargetState {
        TargetState {
            phase,
            power,
            daemon_ready,
            message: message.and_then(DescriptiveText::lossy),
        }
    }

    #[test]
    fn target_states_map_to_lifecycle_verdicts() {
        use TargetPhase::*;
        assert_eq!(
            target_verdict(&state(Provisioning, None, false, None)),
            TargetVerdict::Status(EnvironmentStatus::Provisioning)
        );
        assert_eq!(
            target_verdict(&state(Active, Some(PowerState::Running), false, None)),
            TargetVerdict::Status(EnvironmentStatus::Booting)
        );
        assert_eq!(
            target_verdict(&state(Active, Some(PowerState::Running), true, None)),
            TargetVerdict::DaemonReady
        );
        assert_eq!(
            target_verdict(&state(Active, Some(PowerState::Stopped), false, None)),
            TargetVerdict::Status(EnvironmentStatus::Offline)
        );
        assert_eq!(
            target_verdict(&state(Failed, None, false, Some("disk full"))),
            TargetVerdict::Failed("disk full".to_owned())
        );
        assert!(matches!(
            target_verdict(&state(Closed, None, false, None)),
            TargetVerdict::Failed(_)
        ));
        assert!(matches!(
            target_verdict(&state(Unknown, None, false, None)),
            TargetVerdict::Unknown(_)
        ));
        assert!(matches!(
            target_verdict(&state(Active, None, false, None)),
            TargetVerdict::Unknown(_)
        ));
    }

    #[test]
    fn the_provisioning_deadline_counts_from_the_first_error_or_stall() {
        let error = EnvironmentLifecycleError::record(None, "provider unreachable", 1_000);
        assert!(!provisioning_deadline_passed(None, i64::MAX));
        assert!(!provisioning_deadline_passed(
            Some(&error),
            1_000 + PROVISIONING_DEADLINE_MS - 1
        ));
        let later = EnvironmentLifecycleError::record(Some(&error), "still unreachable", 50_000);
        assert_eq!(later.first_at_ms, 1_000);
        assert!(provisioning_deadline_passed(
            Some(&later),
            1_000 + PROVISIONING_DEADLINE_MS
        ));
    }

    #[test]
    fn stalled_observations_carry_the_provider_message_into_the_deadline() {
        let booting = waiting_note(
            EnvironmentStatus::Booting,
            &state(
                TargetPhase::Active,
                Some(PowerState::Running),
                false,
                Some("guest agent not up"),
            ),
        );
        assert_eq!(
            booting,
            "waiting for envd to answer on the target: guest agent not up"
        );
        let provisioning = waiting_note(
            EnvironmentStatus::Provisioning,
            &state(TargetPhase::Provisioning, None, false, None),
        );
        assert_eq!(
            provisioning,
            "waiting for the provider to provision the target"
        );
        // A stalled observation starts the deadline like a transient error:
        // the run of notes keeps its first instant.
        let first = EnvironmentLifecycleError::record(None, &booting, 1_000);
        let repeated = EnvironmentLifecycleError::record(Some(&first), &booting, 900_000);
        assert!(provisioning_deadline_passed(
            Some(&repeated),
            1_000 + PROVISIONING_DEADLINE_MS
        ));
    }

    #[test]
    fn repeated_errors_are_rewritten_only_when_new_or_stale() {
        let recorded = EnvironmentLifecycleError::record(None, "provider unreachable", 1_000);
        assert!(lifecycle_error_is_news(None, "provider unreachable", 1_000));
        assert!(!lifecycle_error_is_news(
            Some(&recorded),
            "provider unreachable",
            2_000
        ));
        assert!(lifecycle_error_is_news(
            Some(&recorded),
            "provider refused",
            2_000
        ));
        assert!(lifecycle_error_is_news(
            Some(&recorded),
            "provider unreachable",
            1_000 + LIFECYCLE_ERROR_REFRESH_MS
        ));
    }

    async fn created_target(controller: &FakeController) -> (TargetOwner, String, SecretValue) {
        let credential = SecretValue::new(controller.credential());
        let (client, initialized) = environment_client::ControllerClient::connect_initialized(
            controller.controller_url(),
            provider_connect_options(&credential),
        )
        .await
        .expect("controller handshake");
        let owner = TargetOwner {
            universe_id: UniverseId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
            binding_id: BindingId::new("primary").unwrap(),
            environment_id: environment_protocol::ids::EnvironmentId::new("environment_1").unwrap(),
            incarnation_id: IncarnationId::new("incarnation_1").unwrap(),
        };
        let observation = client
            .create_target(&CreateTargetParams {
                owner: owner.clone(),
                template_id: TemplateId::new(
                    crate::environments::fake_controller::FAKE_TEMPLATE_ID,
                )
                .unwrap(),
            })
            .await
            .expect("create")
            .target;
        let target_id = observation.target_id.expect("target id");
        let base: DataRouteBase = initialized.routes.data;
        let endpoint = provider_data_route(&base, &owner, &target_id);
        // Observation is a pure read of the same target.
        let observed = client
            .get_target(&GetTargetParams {
                target: Ref {
                    owner: owner.clone(),
                    target_id: Some(target_id.clone()),
                },
            })
            .await
            .expect("get")
            .target;
        assert_eq!(observed.target_id, Some(target_id));
        let _ = client.close().await;
        (owner, endpoint, credential)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_ready_transition_handshake_records_the_daemon_build() {
        let controller = FakeController::start().await.unwrap();
        let (_, endpoint, credential) = created_target(&controller).await;
        match probe_daemon(&endpoint, &credential).await {
            DaemonProbe::Ready(build) => {
                assert_eq!(build.protocol_version, ENVD_PROTOCOL_VERSION);
                assert!(build.version.is_some());
            }
            other => panic!("expected a ready daemon, got {other:?}"),
        }
        // Without the provider credential the data route is refused.
        assert!(matches!(
            probe_daemon(&endpoint, &SecretValue::new("wrong")).await,
            DaemonProbe::Unreachable(_)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_daemon_of_another_protocol_version_is_a_mismatch_naming_both() {
        let controller = FakeController::start_with(FakeControllerOptions {
            daemon_protocol_version: ENVD_PROTOCOL_VERSION + 1,
            ..FakeControllerOptions::default()
        })
        .await
        .unwrap();
        let (_, endpoint, credential) = created_target(&controller).await;
        assert_eq!(
            probe_daemon(&endpoint, &credential).await,
            DaemonProbe::Mismatch {
                local: ENVD_PROTOCOL_VERSION,
                remote: ENVD_PROTOCOL_VERSION + 1,
            }
        );
    }
}
