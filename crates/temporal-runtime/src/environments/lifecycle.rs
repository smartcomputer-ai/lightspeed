//! Environment lifecycle: the API handlers and the reconciler.
//!
//! Lightspeed keeps durable retries for the two things it alone decides:
//! create until accepted (and opened by one data-plane handshake), and
//! close until gone. Everything a provider does while a target runs —
//! power, idle, wake, ingress — is the provider's; the slow display sweep
//! only records what it reports, and acts only when it reports the target
//! failed or gone. Power and ingress API calls are synchronous
//! pass-throughs that store nothing but the returned observation.

use super::provider_controllers::{
    ControllerDisposition, ControllerError, ControllerFeature, PASS_THROUGH_TIMEOUT,
    ProviderController, ProviderControllerConnector, call_disposition, connect_provider_controller,
    provider_connect_options,
};
use super::providers::{
    environment_view, map_environments_error, parse_environment_provider_id, registry_idle_policy,
    registry_power_state, registry_state,
};
use super::signals::EnvironmentSignals;
use super::*;

use ::environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment, DaemonBuild,
    EndProvisionedEnvironment, EnvironmentCloseReason, EnvironmentEndpoint, EnvironmentId,
    EnvironmentProviderId, EnvironmentProviderStore, EnvironmentProvisionRequestId,
    EnvironmentRecord, EnvironmentRegistryError, EnvironmentState, EnvironmentStore,
    EnvironmentTemplateId, FailEnvironment, FinishCloseEnvironment, ListEnvironments,
    ObservedTarget, OpenEnvironment, RecordTargetObservation, ScheduleEnvironmentAttempt,
    UpdateEnvironment, WAIT_RETRY_AFTER_MS,
};
use environment_client::{ClientError, DataClient};
use environment_protocol::{
    control::{
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            CloseTargetParams, CreateTargetParams, GetTargetParams, SetTargetPowerParams,
            TargetObservation, TargetOwner, TargetPhase, TargetRef,
        },
    },
    ids::{TemplateId, UniverseId},
    manifest::{IngressEnsure, IngressRemove, TargetSetPower},
    metadata::DisplayName,
    routes::{DataRouteRefusal, provider_data_route},
};
use futures_util::{StreamExt, stream};
use std::{collections::BTreeMap, time::Duration};

/// How long a provisioning environment may take, from entering
/// `provisioning` to its open handshake, before it is failed.
pub(crate) const PROVISIONING_DEADLINE_MS: i64 = 30 * 60 * 1000;

/// Re-check interval while the provider reports work in progress.
pub(crate) const POLL_MS: i64 = 2_000;

/// First backoff after a failed attempt; doubled per attempt.
const BACKOFF_BASE_MS: i64 = 2_000;

/// Longest backoff between attempts.
const BACKOFF_MAX_MS: i64 = 5 * 60 * 1000;

/// Rows of one provider in flight at once, over one shared controller
/// connection.
pub(crate) const PROVIDER_CONCURRENCY: usize = 8;

/// Rows one work pass takes from a universe.
pub(crate) const WORK_BATCH: u32 = 256;

/// How often the reconciler wakes without a nudge.
pub(crate) const WORK_TICK: Duration = Duration::from_secs(1);

/// How often the display sweep observes open provisioned environments.
pub(crate) const DISPLAY_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The delay before attempt `attempts + 1` after `attempts` consecutive
/// failures: 2 s doubling, at most 5 min.
pub(crate) fn backoff_delay_ms(attempts: u32) -> i64 {
    let shift = attempts.saturating_sub(1).min(8);
    (BACKOFF_BASE_MS << shift).min(BACKOFF_MAX_MS)
}

/// True once a provisioning row has been provisioning for the deadline.
pub(crate) fn provisioning_deadline_passed(record: &EnvironmentRecord, now_ms: i64) -> bool {
    record.state == EnvironmentState::Provisioning
        && now_ms.saturating_sub(record.state_changed_at_ms) >= PROVISIONING_DEADLINE_MS
}

/// The controller-plane owner of a provisioned environment.
pub(crate) fn target_owner(
    universe_id: uuid::Uuid,
    environment: &EnvironmentRecord,
) -> Result<TargetOwner, EnvironmentRegistryError> {
    let invalid =
        |error: environment_protocol::ids::IdError| EnvironmentRegistryError::InvalidInput {
            message: format!("environment ids do not fit the controller protocol: {error}"),
        };
    Ok(TargetOwner {
        universe_id: UniverseId::new(universe_id.to_string()).map_err(invalid)?,
        environment_id: environment_protocol::ids::EnvironmentId::new(
            environment.environment_id.as_str(),
        )
        .map_err(invalid)?,
    })
}

/// The wire name of a target phase, for state messages.
fn phase_name(phase: TargetPhase) -> &'static str {
    match phase {
        TargetPhase::Provisioning => "provisioning",
        TargetPhase::Active => "active",
        TargetPhase::Closing => "closing",
        TargetPhase::Closed => "closed",
        TargetPhase::Failed => "failed",
        TargetPhase::Unknown => "unknown",
    }
}

/// What a provider observation records: power only on an active target.
fn observed_target(observation: &TargetObservation, now_ms: i64) -> ObservedTarget {
    ObservedTarget {
        phase: observation.phase,
        power: observation
            .power
            .filter(|_| observation.phase == TargetPhase::Active),
        message: observation
            .message
            .as_ref()
            .map(|message| message.as_str().to_owned()),
        observed_at_ms: now_ms,
    }
}

fn observation_request(
    environment: &EnvironmentRecord,
    observation: &TargetObservation,
    now_ms: i64,
) -> RecordTargetObservation {
    RecordTargetObservation {
        environment_id: environment.environment_id.clone(),
        provider_target_id: observation.target_id.clone(),
        observed: observed_target(observation, now_ms),
        public_endpoint: observation
            .ingress
            .as_ref()
            .map(|ingress| ingress.endpoint.as_str().to_owned()),
        observed_at_ms: now_ms,
    }
}

/// Wall-clock milliseconds for reconciler writes.
pub(crate) fn clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl EnvironmentService {
    fn reconciler(&self) -> Reconciler<'_> {
        Reconciler {
            universe_id: self.store.config().universe_id,
            environments: self.store.as_ref(),
            providers: self.store.as_ref(),
            connector: self.provider_controller_connector.as_ref(),
            signals: &self.signals,
        }
    }

    pub(crate) async fn create_external_environment_record(
        &self,
        params: EnvironmentExternalCreateParams,
    ) -> Result<EnvironmentExternalCreateResponse, AgentApiError> {
        let request_id = parse_request_id(params.request_id)?;
        let endpoint = EnvironmentEndpoint::parse(&params.endpoint).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid environment endpoint: {error}"))
        })?;
        let environment = EnvironmentStore::create_external_environment(
            self.store.as_ref(),
            CreateExternalEnvironment {
                request_id,
                environment_id: allocate_environment_id(),
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

    /// `environments/create`: accept the creation and nudge the
    /// reconciler; provider I/O belongs to the reconciler.
    pub(crate) async fn create_environment_record(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentCreateResponse, AgentApiError> {
        let request_id = parse_request_id(params.request_id)?;
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let template_id = EnvironmentTemplateId::try_new(params.template_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid environment template id: {error}"))
        })?;
        let idle = params.idle.as_ref().map(registry_idle_policy).transpose()?;
        let environment = EnvironmentStore::create_environment(
            self.store.as_ref(),
            CreateEnvironment {
                request_id,
                environment_id: allocate_environment_id(),
                provider_id,
                template_id,
                idle,
                display_name: params.display_name,
                metadata: validated_caller_metadata(params.metadata)?,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        if environment.state == EnvironmentState::Provisioning {
            self.signals.nudge_work();
        }
        Ok(EnvironmentCreateResponse {
            environment: environment_view(&environment),
        })
    }

    /// `environments/power/put`: a synchronous `target/setPower`
    /// pass-through. Only the returned observation is stored.
    pub(crate) async fn put_environment_power_record(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<EnvironmentPowerPutResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let power = registry_power_state(params.power);
        let environment = self
            .open_provisioned(&environment_id, "power control")
            .await?;
        let environment = self
            .pass_through(
                &environment,
                ControllerFeature::Power,
                |controller, target| {
                    Box::pin(async move {
                        controller
                            .client
                            .call_with_timeout::<TargetSetPower>(
                                &SetTargetPowerParams { target, power },
                                PASS_THROUGH_TIMEOUT,
                            )
                            .await
                    })
                },
            )
            .await?;
        Ok(EnvironmentPowerPutResponse {
            environment: environment_view(&environment),
        })
    }

    /// `environments/ingress/put`: a synchronous `ingress/ensure` or
    /// `ingress/remove` pass-through; the reported endpoint is cached.
    pub(crate) async fn put_environment_ingress_record(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<EnvironmentIngressPutResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let enabled = params.enabled;
        let environment = self
            .open_provisioned(&environment_id, "provider-managed ingress")
            .await?;
        let environment = self
            .pass_through(
                &environment,
                ControllerFeature::Ingress,
                |controller, target| {
                    Box::pin(async move {
                        if enabled {
                            controller
                                .client
                                .call_with_timeout::<IngressEnsure>(
                                    &EnsureIngressParams { target },
                                    PASS_THROUGH_TIMEOUT,
                                )
                                .await
                        } else {
                            controller
                                .client
                                .call_with_timeout::<IngressRemove>(
                                    &RemoveIngressParams { target },
                                    PASS_THROUGH_TIMEOUT,
                                )
                                .await
                        }
                    })
                },
            )
            .await?;
        Ok(EnvironmentIngressPutResponse {
            environment: environment_view(&environment),
        })
    }

    /// The environment a pass-through acts on: provisioned and open.
    async fn open_provisioned(
        &self,
        environment_id: &EnvironmentId,
        what: &str,
    ) -> Result<EnvironmentRecord, AgentApiError> {
        let environment = EnvironmentStore::read_environment(self.store.as_ref(), environment_id)
            .await
            .map_err(map_environments_error)?;
        if environment.provisioned().is_none() {
            return Err(AgentApiError::rejected(format!(
                "{what} is available only for provisioned environments"
            )));
        }
        if environment.state != EnvironmentState::Open {
            return Err(AgentApiError::rejected(format!(
                "environment {environment_id} is {}; {what} needs an open environment",
                environment.state
            )));
        }
        Ok(environment)
    }

    /// Run one synchronous controller call against an open provisioned
    /// environment's target and record the observation it answers.
    async fn pass_through<F>(
        &self,
        environment: &EnvironmentRecord,
        feature: ControllerFeature,
        call: F,
    ) -> Result<EnvironmentRecord, AgentApiError>
    where
        F: for<'a> FnOnce(
            &'a ProviderController,
            TargetRef,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<environment_protocol::control::target::TargetResponse, ClientError>,
        >,
    {
        let source = environment
            .provisioned()
            .ok_or_else(|| AgentApiError::internal("pass-through on a non-provisioned row"))?;
        let provider_id = source.provider_id.clone();
        let target_id = source.provider_target_id.clone().ok_or_else(|| {
            AgentApiError::rejected(format!(
                "environment {} has no provider target",
                environment.environment_id
            ))
        })?;
        let owner = target_owner(self.store.config().universe_id, environment)
            .map_err(map_environments_error)?;
        let controller = self
            .connect_provider_controller(&provider_id)
            .await
            .map_err(ControllerError::into_api_error)?;
        let answered = match controller.require(feature) {
            Ok(()) => call(
                &controller,
                TargetRef {
                    owner: owner.clone(),
                    target_id: Some(target_id),
                },
            )
            .await
            .map_err(|error| controller.call_error(error)),
            Err(error) => Err(error),
        };
        controller.close().await;
        let observation = answered.map_err(ControllerError::into_api_error)?.target;
        if observation.owner != owner {
            return Err(AgentApiError::internal(format!(
                "provider {provider_id} answered with another owner's target"
            )));
        }
        EnvironmentStore::record_target_observation(
            self.store.as_ref(),
            observation_request(environment, &observation, now_ms()?),
        )
        .await
        .map_err(map_environments_error)
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

    /// `environments/update`: replace the display name and caller metadata.
    pub(crate) async fn update_environment_record(
        &self,
        params: EnvironmentUpdateParams,
    ) -> Result<EnvironmentUpdateResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let environment = EnvironmentStore::update_environment(
            self.store.as_ref(),
            UpdateEnvironment {
                environment_id,
                display_name: params.display_name,
                metadata: validated_caller_metadata(params.metadata)?,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentUpdateResponse {
            environment: environment_view(&environment),
        })
    }

    pub(crate) async fn list_environment_records(
        &self,
        params: EnvironmentListParams,
    ) -> Result<EnvironmentListResponse, AgentApiError> {
        let page = EnvironmentStore::list_environments(
            self.store.as_ref(),
            ListEnvironments {
                provider_id: params
                    .provider_id
                    .map(parse_environment_provider_id)
                    .transpose()?,
                registration_key_id: params
                    .registration_key_id
                    .map(parse_registration_key_id)
                    .transpose()?,
                state: params.state.map(registry_state),
                include_closed: params.include_closed,
                metadata: params.metadata,
                limit: params
                    .limit
                    .unwrap_or(::environments::LIST_ENVIRONMENTS_DEFAULT_LIMIT),
                after: params
                    .after
                    .map(parse_registry_environment_id)
                    .transpose()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentListResponse {
            environments: page.environments.iter().map(environment_view).collect(),
            next_after: page.next_after.map(|id| id.to_string()),
        })
    }

    /// `environments/close`: a provisioned environment moves to `closing`
    /// and the reconciler is nudged; external and registered ones close at
    /// once and their live routes end.
    pub(crate) async fn close_environment_record(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<EnvironmentCloseResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let environment = self
            .begin_close(&environment_id, EnvironmentCloseReason::Requested)
            .await?;
        Ok(EnvironmentCloseResponse {
            environment: environment_view(&environment),
        })
    }

    /// Begin closing one environment with `reason`, then signal: the
    /// reconciler for a provisioned row, live routes for every row.
    pub(crate) async fn begin_close(
        &self,
        environment_id: &EnvironmentId,
        reason: EnvironmentCloseReason,
    ) -> Result<EnvironmentRecord, AgentApiError> {
        let environment = EnvironmentStore::begin_close_environment(
            self.store.as_ref(),
            BeginCloseEnvironment {
                environment_id: environment_id.clone(),
                reason,
                at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        if environment.state == EnvironmentState::Closing {
            self.signals.nudge_work();
        }
        self.signals
            .revoke(self.store.config().universe_id, environment_id.as_str());
        Ok(environment)
    }

    /// Start a work pass, as its own task, for every provider of this
    /// universe with due provisioning or closing rows and no pass in
    /// flight; returns how many it started. The caller never waits on a
    /// provider, so a slow or hung controller delays only its own rows.
    pub(crate) async fn start_environment_work(
        &self,
        now_ms: i64,
        work: &ProviderWork,
    ) -> Result<usize, AgentApiError> {
        let universe_id = self.store.config().universe_id;
        let rows = self
            .store
            .list_environment_work(now_ms, WORK_BATCH)
            .await
            .map_err(map_environments_error)?;
        let mut started = 0;
        for (provider_id, rows) in by_provider(rows) {
            let Some(claim) = work.claim(universe_id, &provider_id) else {
                continue;
            };
            let service = self.clone();
            tokio::spawn(async move {
                let outcome = service
                    .reconciler()
                    .work_provider(provider_id, rows, now_ms)
                    .await;
                claim.finish(outcome.into_result());
            });
            started += 1;
        }
        Ok(started)
    }

    /// One display sweep over this universe's open provisioned rows.
    pub(crate) async fn run_display_sweep(&self, now_ms: i64) -> Result<usize, AgentApiError> {
        self.reconciler().run_display_sweep(now_ms).await
    }

    /// One work pass that treats every scheduled row as due, then the
    /// display sweep of this universe; acceptance tests drive the
    /// reconciler with it instead of the background loop.
    pub async fn reconcile_environments_once(&self) -> Result<usize, AgentApiError> {
        let reconciler = self.reconciler();
        let worked = reconciler.run_work(i64::MAX, clock_ms()).await;
        let swept = reconciler.run_display_sweep(clock_ms()).await;
        Ok(worked? + swept?)
    }
}

/// The reconciler of one universe, over its stores.
pub(crate) struct Reconciler<'a> {
    pub(crate) universe_id: uuid::Uuid,
    pub(crate) environments: &'a dyn EnvironmentStore,
    pub(crate) providers: &'a dyn EnvironmentProviderStore,
    pub(crate) connector: &'a dyn ProviderControllerConnector,
    pub(crate) signals: &'a EnvironmentSignals,
}

/// What one pass over a group of rows did.
#[derive(Default)]
struct PassOutcome {
    changed: usize,
    first_error: Option<AgentApiError>,
}

impl PassOutcome {
    fn absorb(&mut self, environment_id: &EnvironmentId, result: Result<bool, AgentApiError>) {
        match result {
            Ok(changed) => self.changed += usize::from(changed),
            Err(error) => {
                tracing::debug!(
                    target: "temporal_runtime",
                    %environment_id,
                    %error,
                    "environment reconcile step failed"
                );
                self.first_error.get_or_insert(AgentApiError {
                    message: format!("environment {environment_id}: {}", error.message),
                    ..error
                });
            }
        }
    }

    fn merge(&mut self, other: PassOutcome) {
        self.changed += other.changed;
        if self.first_error.is_none() {
            self.first_error = other.first_error;
        }
    }

    fn into_result(self) -> Result<usize, AgentApiError> {
        match self.first_error {
            Some(error) => Err(error),
            None => Ok(self.changed),
        }
    }
}

/// Group rows by provider; rows without one are not provisioned.
fn by_provider(
    rows: Vec<EnvironmentRecord>,
) -> BTreeMap<EnvironmentProviderId, Vec<EnvironmentRecord>> {
    let mut groups: BTreeMap<EnvironmentProviderId, Vec<EnvironmentRecord>> = BTreeMap::new();
    for row in rows {
        if let Some(provider_id) = row.provider_id().cloned() {
            groups.entry(provider_id).or_default().push(row);
        }
    }
    groups
}

/// How a step's registry failure is treated: the row's own records that
/// cannot be written (a contract breach the provider caused) back off like
/// a transient error; a missing row is done; a store failure is the pass's.
fn step_registry_error(error: &EnvironmentRegistryError) -> Option<String> {
    match error {
        EnvironmentRegistryError::InvalidInput { message } => Some(message.clone()),
        EnvironmentRegistryError::AlreadyExists { kind, id } => {
            Some(format!("the provider's answer conflicts with {kind} {id}"))
        }
        _ => None,
    }
}

impl Reconciler<'_> {
    /// Work on the rows due at `list_at_ms`; `now_ms` stamps writes and
    /// decides deadlines and backoff. Providers run in parallel, each with
    /// at most [`PROVIDER_CONCURRENCY`] rows in flight over one controller
    /// connection.
    pub(crate) async fn run_work(
        &self,
        list_at_ms: i64,
        now_ms: i64,
    ) -> Result<usize, AgentApiError> {
        let rows = self
            .environments
            .list_environment_work(list_at_ms, WORK_BATCH)
            .await
            .map_err(map_environments_error)?;
        let groups = by_provider(rows);
        let outcomes = futures_util::future::join_all(
            groups
                .into_iter()
                .map(|(provider_id, rows)| self.work_provider(provider_id, rows, now_ms)),
        )
        .await;
        let mut outcome = PassOutcome::default();
        for provider in outcomes {
            outcome.merge(provider);
        }
        outcome.into_result()
    }

    async fn work_provider(
        &self,
        provider_id: EnvironmentProviderId,
        rows: Vec<EnvironmentRecord>,
        now_ms: i64,
    ) -> PassOutcome {
        let mut outcome = PassOutcome::default();
        let controller =
            match connect_provider_controller(self.providers, self.connector, &provider_id).await {
                Ok(controller) => Some(controller),
                Err(error) => {
                    let unregistered = matches!(
                        error,
                        ControllerError::Registry(EnvironmentRegistryError::NotFound { .. })
                    );
                    let message = error.to_string();
                    for row in &rows {
                        let result = self
                            .without_controller(row, unregistered, &message, now_ms)
                            .await;
                        outcome.absorb(&row.environment_id, result);
                    }
                    None
                }
            };
        let Some(controller) = controller else {
            return outcome;
        };
        let controller = &controller;
        let results = stream::iter(rows)
            .map(|row| async move {
                let result = self.step(controller, &row, now_ms).await;
                (row.environment_id, result)
            })
            .buffer_unordered(PROVIDER_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        controller.close().await;
        for (environment_id, result) in results {
            outcome.absorb(&environment_id, result);
        }
        outcome
    }

    /// A due row whose provider controller could not be reached. A closing
    /// row of a provider that is no longer registered is closed: nothing
    /// can reach its target any more.
    async fn without_controller(
        &self,
        row: &EnvironmentRecord,
        unregistered: bool,
        message: &str,
        now_ms: i64,
    ) -> Result<bool, AgentApiError> {
        let result = match row.state {
            EnvironmentState::Closing if unregistered => {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment_id = %row.environment_id,
                    "closing an environment whose provider is no longer registered"
                );
                self.finish_close(row, now_ms).await
            }
            EnvironmentState::Provisioning if provisioning_deadline_passed(row, now_ms) => {
                self.deadline(row, now_ms).await
            }
            EnvironmentState::Provisioning if unregistered => {
                self.fail(row, message, None, now_ms).await
            }
            _ => self.backoff(row, message, now_ms).await,
        };
        result.map_err(map_environments_error)
    }

    async fn step(
        &self,
        controller: &ProviderController,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, AgentApiError> {
        let attempt = match row.state {
            EnvironmentState::Provisioning => self.provision_step(controller, row, now_ms).await,
            EnvironmentState::Closing => self.close_step(controller, row, now_ms).await,
            _ => Ok(false),
        };
        match attempt {
            Ok(changed) => Ok(changed),
            Err(EnvironmentRegistryError::NotFound { .. }) => Ok(false),
            Err(error) => match step_registry_error(&error) {
                Some(message) => self
                    .backoff(row, &message, now_ms)
                    .await
                    .map_err(map_environments_error),
                None => Err(map_environments_error(error)),
            },
        }
    }

    /// One step of a provisioning row: create while no target id is
    /// recorded, observe once one is, and open after one data-plane
    /// handshake with the target's envd.
    async fn provision_step(
        &self,
        controller: &ProviderController,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        if provisioning_deadline_passed(row, now_ms) {
            return self.deadline(row, now_ms).await;
        }
        let Some(source) = row.provisioned() else {
            return Ok(false);
        };
        let owner = target_owner(self.universe_id, row)?;
        let answer = match &source.provider_target_id {
            Some(target_id) => {
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
            None => {
                let template_id =
                    TemplateId::new(source.template_id.as_str()).map_err(|error| {
                        EnvironmentRegistryError::InvalidInput {
                            message: format!("template id does not fit the protocol: {error}"),
                        }
                    })?;
                controller
                    .client
                    .create_target(&CreateTargetParams {
                        owner: owner.clone(),
                        template_id,
                        display_name: row
                            .display_name
                            .as_deref()
                            .and_then(|name| DisplayName::new(name).ok()),
                        idle: source.idle.clone(),
                    })
                    .await
            }
        };
        let observation = match answer {
            Ok(response) => response.target,
            Err(error) => {
                let error = controller.call_error(error);
                return match error.disposition() {
                    ControllerDisposition::Transient => {
                        self.backoff(row, &error.to_string(), now_ms).await
                    }
                    ControllerDisposition::Refused => {
                        self.fail(row, &error.to_string(), None, now_ms).await
                    }
                    ControllerDisposition::Gone => {
                        let message = if source.provider_target_id.is_some() {
                            format!("the provider target disappeared: {error}")
                        } else {
                            error.to_string()
                        };
                        self.fail(row, &message, None, now_ms).await
                    }
                };
            }
        };
        if observation.owner != owner {
            return self
                .backoff(
                    row,
                    &format!(
                        "provider {} answered with another owner's target",
                        controller.provider_id
                    ),
                    now_ms,
                )
                .await;
        }
        let recorded = self
            .environments
            .record_target_observation(observation_request(row, &observation, now_ms))
            .await?;
        // A close that landed while this step was in flight owns the row's
        // schedule now; its own step picks the target up next.
        if recorded.state != EnvironmentState::Provisioning {
            return Ok(true);
        }
        let message = observation.message.as_ref().map(|message| message.as_str());
        match observation.phase {
            TargetPhase::Provisioning => {
                let note = match message {
                    Some(message) => format!("waiting for the provider: {message}"),
                    None => "waiting for the provider".to_owned(),
                };
                self.schedule(&recorded, now_ms + POLL_MS, 0, Some(note), now_ms)
                    .await
            }
            TargetPhase::Active => {
                self.open_handshake(controller, &recorded, &owner, now_ms)
                    .await
            }
            TargetPhase::Failed => {
                self.fail(
                    &recorded,
                    message.unwrap_or("the provider reported the target failed"),
                    None,
                    now_ms,
                )
                .await
            }
            TargetPhase::Closing | TargetPhase::Closed => {
                self.fail(
                    &recorded,
                    "the provider ended the target before it opened",
                    None,
                    now_ms,
                )
                .await
            }
            TargetPhase::Unknown => {
                self.backoff(
                    &recorded,
                    message.unwrap_or("the provider cannot tell the target's state"),
                    now_ms,
                )
                .await
            }
        }
    }

    /// The only readiness proof: `initialize` over the target's data route.
    /// A typed refusal that may clear is waited out at its `Retry-After`.
    async fn open_handshake(
        &self,
        controller: &ProviderController,
        row: &EnvironmentRecord,
        owner: &TargetOwner,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let Some(target_id) = row
            .provisioned()
            .and_then(|source| source.provider_target_id.clone())
        else {
            return self
                .backoff(
                    row,
                    "the provider reported an active target without a targetId",
                    now_ms,
                )
                .await;
        };
        let route = provider_data_route(&controller.data_route_base, owner, &target_id);
        match DataClient::connect_initialized(
            &route,
            provider_connect_options(controller.credential.as_ref()),
        )
        .await
        {
            Ok((client, initialized)) => {
                let _ = client.close().await;
                let written = self
                    .environments
                    .open_environment(OpenEnvironment {
                        environment_id: row.environment_id.clone(),
                        daemon_build: DaemonBuild::from_implementation(
                            &initialized.server_info,
                            initialized.protocol_version,
                        ),
                        at_ms: now_ms,
                    })
                    .await?;
                Ok(written.state == EnvironmentState::Open)
            }
            Err(ClientError::VersionMismatch { local, remote, .. }) => {
                self.fail(
                    row,
                    &format!(
                        "the environment's envd speaks protocol version {remote}; this runtime requires {local}"
                    ),
                    Some(DaemonBuild {
                        version: None,
                        git_sha: None,
                        protocol_version: remote,
                    }),
                    now_ms,
                )
                .await
            }
            Err(error) => match error.data_route_refusal() {
                Some(refusal) if refusal.is_retryable() => {
                    let retry_after = i64::try_from(
                        error.retry_after_ms().unwrap_or(WAIT_RETRY_AFTER_MS),
                    )
                    .unwrap_or(i64::MAX);
                    self.schedule(
                        row,
                        now_ms.saturating_add(retry_after),
                        0,
                        Some(format!("waiting for envd: {refusal}")),
                        now_ms,
                    )
                    .await
                }
                Some(DataRouteRefusal::Gone {}) => {
                    self.fail(row, "the provider closed the target", None, now_ms)
                        .await
                }
                Some(DataRouteRefusal::Failed { message }) => {
                    self.fail(row, message.as_str(), None, now_ms).await
                }
                _ => {
                    self.backoff(
                        row,
                        &format!("envd did not complete the data-plane handshake: {error}"),
                        now_ms,
                    )
                    .await
                }
            },
        }
    }

    /// One step of a closing row: `target/close` by owner, with the target
    /// id when one is recorded, until the provider reports it gone. Closing
    /// has no deadline: a provider still closing is polled, and any other
    /// phase it answers (a target it cannot remove) backs off like an
    /// error.
    async fn close_step(
        &self,
        controller: &ProviderController,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let owner = target_owner(self.universe_id, row)?;
        let target_id = row
            .provisioned()
            .and_then(|source| source.provider_target_id.clone());
        match controller
            .client
            .close_target(&CloseTargetParams {
                target: TargetRef { owner, target_id },
            })
            .await
        {
            Ok(response) => match response.target.phase {
                TargetPhase::Closed => self.finish_close(row, now_ms).await,
                TargetPhase::Closing => {
                    self.schedule(
                        row,
                        now_ms + POLL_MS,
                        0,
                        Some("waiting for the provider to close the target".to_owned()),
                        now_ms,
                    )
                    .await
                }
                phase => {
                    let message = match response.target.message.as_ref() {
                        Some(message) => format!(
                            "the provider answered close with a {} target: {}",
                            phase_name(phase),
                            message.as_str()
                        ),
                        None => format!(
                            "the provider answered close with a {} target",
                            phase_name(phase)
                        ),
                    };
                    self.backoff(row, &message, now_ms).await
                }
            },
            Err(error) if call_disposition(&error) == ControllerDisposition::Gone => {
                self.finish_close(row, now_ms).await
            }
            Err(error) => {
                let error = controller.call_error(error);
                self.backoff(row, &error.to_string(), now_ms).await
            }
        }
    }

    /// Observe every open provisioned row with `target/get` and record what
    /// the provider reports. A failed target fails its row; a closed or
    /// missing one closes it (`provider_ended`). Power is never touched.
    pub(crate) async fn run_display_sweep(&self, now_ms: i64) -> Result<usize, AgentApiError> {
        let rows = self
            .environments
            .list_open_provisioned_environments()
            .await
            .map_err(map_environments_error)?;
        let outcomes = futures_util::future::join_all(
            by_provider(rows)
                .into_iter()
                .map(|(provider_id, rows)| self.sweep_provider(provider_id, rows, now_ms)),
        )
        .await;
        let mut outcome = PassOutcome::default();
        for provider in outcomes {
            outcome.merge(provider);
        }
        outcome.into_result()
    }

    async fn sweep_provider(
        &self,
        provider_id: EnvironmentProviderId,
        rows: Vec<EnvironmentRecord>,
        now_ms: i64,
    ) -> PassOutcome {
        let mut outcome = PassOutcome::default();
        let controller =
            match connect_provider_controller(self.providers, self.connector, &provider_id).await {
                Ok(controller) => controller,
                Err(error) => {
                    tracing::debug!(
                        target: "temporal_runtime",
                        %provider_id,
                        %error,
                        "display sweep skipped an unreachable provider"
                    );
                    return outcome;
                }
            };
        let controller = &controller;
        let results = stream::iter(rows)
            .map(|row| async move {
                let result = self.observe_open(controller, &row, now_ms).await;
                (row.environment_id, result)
            })
            .buffer_unordered(PROVIDER_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        controller.close().await;
        for (environment_id, result) in results {
            outcome.absorb(
                &environment_id,
                match result {
                    Err(EnvironmentRegistryError::NotFound { .. }) => Ok(false),
                    other => other.map_err(map_environments_error),
                },
            );
        }
        outcome
    }

    async fn observe_open(
        &self,
        controller: &ProviderController,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let Some(target_id) = row
            .provisioned()
            .and_then(|source| source.provider_target_id.clone())
        else {
            return Ok(false);
        };
        let owner = target_owner(self.universe_id, row)?;
        let observation = match controller
            .client
            .get_target(&GetTargetParams {
                target: TargetRef {
                    owner: owner.clone(),
                    target_id: Some(target_id),
                },
            })
            .await
        {
            Ok(response) => response.target,
            Err(error) if call_disposition(&error) == ControllerDisposition::Gone => {
                return self.end(row, now_ms).await;
            }
            Err(error) => {
                tracing::debug!(
                    target: "temporal_runtime",
                    environment_id = %row.environment_id,
                    %error,
                    "display sweep could not observe a target"
                );
                return Ok(false);
            }
        };
        if observation.owner != owner {
            return Ok(false);
        }
        let recorded = self
            .environments
            .record_target_observation(observation_request(row, &observation, now_ms))
            .await?;
        match observation.phase {
            TargetPhase::Failed => {
                let message = observation
                    .message
                    .as_ref()
                    .map(|message| message.as_str())
                    .unwrap_or("the provider reported the target failed");
                self.fail(&recorded, message, None, now_ms).await
            }
            TargetPhase::Closed => self.end(&recorded, now_ms).await,
            _ => Ok(recorded != *row),
        }
    }

    async fn deadline(
        &self,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let message = format!(
            "provisioning did not finish within {} minutes: {}",
            PROVISIONING_DEADLINE_MS / 60_000,
            row.state_message.as_deref().unwrap_or("no progress")
        );
        self.fail(row, &message, None, now_ms).await
    }

    async fn backoff(
        &self,
        row: &EnvironmentRecord,
        message: &str,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let attempts = row
            .provisioned()
            .map_or(0, |source| source.attempts)
            .saturating_add(1);
        self.schedule(
            row,
            now_ms.saturating_add(backoff_delay_ms(attempts)),
            attempts,
            Some(message.to_owned()),
            now_ms,
        )
        .await
    }

    async fn schedule(
        &self,
        row: &EnvironmentRecord,
        next_attempt_at_ms: i64,
        attempts: u32,
        state_message: Option<String>,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let written = self
            .environments
            .schedule_environment_attempt(ScheduleEnvironmentAttempt {
                environment_id: row.environment_id.clone(),
                expected_state: row.state,
                next_attempt_at_ms,
                attempts,
                state_message,
                at_ms: now_ms,
            })
            .await?;
        Ok(written != *row)
    }

    async fn fail(
        &self,
        row: &EnvironmentRecord,
        message: &str,
        daemon_build: Option<DaemonBuild>,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let written = self
            .environments
            .fail_environment(FailEnvironment {
                environment_id: row.environment_id.clone(),
                message: message.to_owned(),
                daemon_build,
                at_ms: now_ms,
            })
            .await?;
        Ok(written.state == EnvironmentState::Failed)
    }

    async fn finish_close(
        &self,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let written = self
            .environments
            .finish_close_environment(FinishCloseEnvironment {
                environment_id: row.environment_id.clone(),
                at_ms: now_ms,
            })
            .await?;
        self.signals
            .revoke(self.universe_id, row.environment_id.as_str());
        Ok(written.state == EnvironmentState::Closed)
    }

    async fn end(
        &self,
        row: &EnvironmentRecord,
        now_ms: i64,
    ) -> Result<bool, EnvironmentRegistryError> {
        let written = self
            .environments
            .end_provisioned_environment(EndProvisionedEnvironment {
                environment_id: row.environment_id.clone(),
                at_ms: now_ms,
            })
            .await?;
        self.signals
            .revoke(self.universe_id, row.environment_id.as_str());
        Ok(written.state == EnvironmentState::Closed)
    }
}

/// The provider work passes in flight, one per universe and provider at a
/// time, and the rate limit of their failure warnings.
#[derive(Clone, Default)]
pub(crate) struct ProviderWork {
    inner: std::sync::Arc<std::sync::Mutex<ProviderWorkState>>,
}

#[derive(Default)]
struct ProviderWorkState {
    running: std::collections::HashSet<(uuid::Uuid, EnvironmentProviderId)>,
    failures: std::collections::HashMap<
        (uuid::Uuid, EnvironmentProviderId),
        (String, std::time::Instant),
    >,
}

impl ProviderWork {
    /// Claim the provider's next pass in the universe; `None` while one is
    /// in flight.
    fn claim(
        &self,
        universe_id: uuid::Uuid,
        provider_id: &EnvironmentProviderId,
    ) -> Option<ProviderWorkClaim> {
        let key = (universe_id, provider_id.clone());
        let mut state = self.inner.lock().expect("provider work poisoned");
        if !state.running.insert(key.clone()) {
            return None;
        }
        Some(ProviderWorkClaim {
            work: self.clone(),
            key,
        })
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self.inner
            .lock()
            .expect("provider work poisoned")
            .running
            .len()
    }
}

/// A claimed provider pass; released when dropped, so a pass that panics
/// still frees its provider.
struct ProviderWorkClaim {
    work: ProviderWork,
    key: (uuid::Uuid, EnvironmentProviderId),
}

impl ProviderWorkClaim {
    /// Record the pass's outcome: a failure warns when it is new or its
    /// last warning is older than [`ReconcileFailureLog::REPEAT_INTERVAL`].
    fn finish(self, result: Result<usize, AgentApiError>) {
        let (universe_id, provider_id) = (&self.key.0, &self.key.1);
        let mut state = self.work.inner.lock().expect("provider work poisoned");
        match result {
            Ok(_) => {
                state.failures.remove(&self.key);
            }
            Err(error) => {
                let message = error.to_string();
                let now = std::time::Instant::now();
                let repeat = state.failures.get(&self.key).is_some_and(|(last, at)| {
                    *last == message
                        && now.duration_since(*at) < ReconcileFailureLog::REPEAT_INTERVAL
                });
                if repeat {
                    tracing::debug!(target: "temporal_runtime", %universe_id, %provider_id, error = %message, "environment provider work pass still failing");
                } else {
                    tracing::warn!(target: "temporal_runtime", %universe_id, %provider_id, error = %message, "environment provider work pass failed (repeats suppressed for 60s)");
                    state.failures.insert(self.key.clone(), (message, now));
                }
            }
        }
        drop(state);
    }
}

impl Drop for ProviderWorkClaim {
    fn drop(&mut self) {
        if let Ok(mut state) = self.work.inner.lock() {
            state.running.remove(&self.key);
        }
    }
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

fn parse_request_id(value: String) -> Result<EnvironmentProvisionRequestId, AgentApiError> {
    EnvironmentProvisionRequestId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid environment request id: {error}"))
    })
}

pub(crate) fn allocate_environment_id() -> EnvironmentId {
    EnvironmentId::new(format!("environment_{}", uuid::Uuid::new_v4().simple()))
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
    use crate::environments::{
        fake_controller::{FAKE_TEMPLATE_ID, FakeController, FakeControllerOptions},
        provider_controllers::WebSocketProviderControllerConnector,
    };
    use ::environments::{
        EnvironmentProviderBindingStore, InMemoryEnvironmentRegistryStore, PowerState,
        ProviderCredentialChange, PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };
    use auth::SecretValue;
    use environment_protocol::ENVD_PROTOCOL_VERSION;

    #[test]
    fn backoff_doubles_from_two_seconds_up_to_five_minutes() {
        assert_eq!(backoff_delay_ms(1), 2_000);
        assert_eq!(backoff_delay_ms(2), 4_000);
        assert_eq!(backoff_delay_ms(5), 32_000);
        assert_eq!(backoff_delay_ms(8), 256_000);
        assert_eq!(backoff_delay_ms(9), BACKOFF_MAX_MS);
        assert_eq!(backoff_delay_ms(u32::MAX), BACKOFF_MAX_MS);
    }

    #[test]
    fn one_pass_per_universe_and_provider_is_in_flight_and_a_dropped_claim_frees_it() {
        let work = ProviderWork::default();
        let universe = uuid::Uuid::from_u128(1);
        let provider = EnvironmentProviderId::new("incus");
        let other = EnvironmentProviderId::new("sandbox");
        let claim = work.claim(universe, &provider).unwrap();
        assert!(work.claim(universe, &provider).is_none());
        let elsewhere = work.claim(uuid::Uuid::from_u128(2), &provider).unwrap();
        let sibling = work.claim(universe, &other).unwrap();
        assert_eq!(work.in_flight(), 3);
        claim.finish(Ok(0));
        drop(elsewhere);
        drop(sibling);
        assert_eq!(work.in_flight(), 0);
        assert!(work.claim(universe, &provider).is_some());
    }

    struct Fixture {
        store: InMemoryEnvironmentRegistryStore,
        signals: EnvironmentSignals,
        provider_id: EnvironmentProviderId,
    }

    impl Fixture {
        async fn new(controller: &FakeController) -> Self {
            Self::with_endpoint(controller.controller_url(), controller.credential()).await
        }

        async fn with_endpoint(endpoint: &str, credential: Option<&str>) -> Self {
            let store = InMemoryEnvironmentRegistryStore::new();
            let provider_id = EnvironmentProviderId::new("fake");
            store
                .put_provider(PutEnvironmentProvider {
                    provider_id: provider_id.clone(),
                    display_name: None,
                    controller_endpoint: EnvironmentEndpoint::parse(endpoint).unwrap(),
                    data_route_base: None,
                    credential: match credential {
                        Some(credential) => {
                            ProviderCredentialChange::Set(SecretValue::new(credential))
                        }
                        None => ProviderCredentialChange::Keep,
                    },
                    updated_at_ms: 1,
                })
                .await
                .unwrap();
            store
                .put_provider_binding(PutEnvironmentProviderBinding {
                    universe_id: store.universe_id(),
                    provider_id: provider_id.clone(),
                    enabled: true,
                    updated_at_ms: 1,
                })
                .await
                .unwrap();
            Self {
                store,
                signals: EnvironmentSignals::default(),
                provider_id,
            }
        }

        fn reconciler(&self) -> Reconciler<'_> {
            Reconciler {
                universe_id: self.store.universe_id(),
                environments: &self.store,
                providers: &self.store,
                connector: &WebSocketProviderControllerConnector,
                signals: &self.signals,
            }
        }

        async fn create(&self, name: &str, created_at_ms: i64) -> EnvironmentId {
            let environment_id = EnvironmentId::new(name);
            self.store
                .create_environment(CreateEnvironment {
                    request_id: EnvironmentProvisionRequestId::new(format!("request-{name}")),
                    environment_id: environment_id.clone(),
                    provider_id: self.provider_id.clone(),
                    template_id: EnvironmentTemplateId::new(FAKE_TEMPLATE_ID),
                    idle: None,
                    display_name: Some("Test environment".to_owned()),
                    metadata: BTreeMap::new(),
                    created_at_ms,
                })
                .await
                .unwrap();
            environment_id
        }

        async fn read(&self, environment_id: &EnvironmentId) -> EnvironmentRecord {
            self.store.read_environment(environment_id).await.unwrap()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_created_target_opens_after_one_handshake_that_records_the_build() {
        let controller = FakeController::start().await.unwrap();
        let fixture = Fixture::new(&controller).await;
        let environment_id = fixture.create("environment_a", 1_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        let opened = fixture.read(&environment_id).await;
        assert_eq!(opened.state, EnvironmentState::Open);
        let source = opened.provisioned().unwrap();
        assert!(source.provider_target_id.is_some());
        assert_eq!(source.next_attempt_at_ms, None);
        assert_eq!(
            source.observed.as_ref().map(|observed| observed.phase),
            Some(TargetPhase::Active)
        );
        assert_eq!(
            opened.daemon_build.map(|build| build.protocol_version),
            Some(ENVD_PROTOCOL_VERSION)
        );
        assert_eq!(controller.target_count().await, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_booting_refusal_is_waited_out_at_its_retry_after() {
        let controller = FakeController::start_with(FakeControllerOptions {
            booting_dials: 1,
            ..FakeControllerOptions::default()
        })
        .await
        .unwrap();
        let fixture = Fixture::new(&controller).await;
        let environment_id = fixture.create("environment_a", 1_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        let waiting = fixture.read(&environment_id).await;
        assert_eq!(waiting.state, EnvironmentState::Provisioning);
        assert_eq!(
            waiting.state_message.as_deref(),
            Some("waiting for envd: booting")
        );
        let source = waiting.provisioned().unwrap();
        assert_eq!(source.attempts, 0, "a typed wait is not a failure");
        assert_eq!(source.next_attempt_at_ms, Some(3_000));
        // Not due before its Retry-After.
        fixture.reconciler().run_work(2_500, 2_500).await.unwrap();
        assert_eq!(
            fixture.read(&environment_id).await.state,
            EnvironmentState::Provisioning
        );
        fixture.reconciler().run_work(3_000, 3_000).await.unwrap();
        assert_eq!(
            fixture.read(&environment_id).await.state,
            EnvironmentState::Open
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unreachable_provider_backs_off_per_row_until_the_deadline() {
        let controller = FakeController::start().await.unwrap();
        controller.set_reachable(false);
        let fixture = Fixture::new(&controller).await;
        let environment_id = fixture.create("environment_a", 1_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        let first = fixture.read(&environment_id).await;
        let source = first.provisioned().unwrap();
        assert_eq!(first.state, EnvironmentState::Provisioning);
        assert_eq!(source.attempts, 1);
        assert_eq!(source.next_attempt_at_ms, Some(4_000));
        assert!(
            first
                .state_message
                .as_deref()
                .unwrap()
                .contains("unreachable")
        );
        fixture
            .reconciler()
            .run_work(i64::MAX, 4_000)
            .await
            .unwrap();
        let second = fixture.read(&environment_id).await;
        assert_eq!(second.provisioned().unwrap().attempts, 2);
        assert_eq!(
            second.provisioned().unwrap().next_attempt_at_ms,
            Some(8_000)
        );
        // Past the deadline the row fails with the last error.
        fixture
            .reconciler()
            .run_work(i64::MAX, 1_000 + PROVISIONING_DEADLINE_MS)
            .await
            .unwrap();
        let failed = fixture.read(&environment_id).await;
        assert_eq!(failed.state, EnvironmentState::Failed);
        assert!(
            failed
                .state_message
                .as_deref()
                .unwrap()
                .starts_with("provisioning did not finish within 30 minutes: ")
        );
        // The provider back: a new row opens.
        controller.set_reachable(true);
        let next = fixture.create("environment_b", 2_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 3_000)
            .await
            .unwrap();
        assert_eq!(fixture.read(&next).await.state, EnvironmentState::Open);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_daemon_of_another_protocol_version_fails_naming_both() {
        let controller = FakeController::start_with(FakeControllerOptions {
            daemon_protocol_version: ENVD_PROTOCOL_VERSION + 1,
            ..FakeControllerOptions::default()
        })
        .await
        .unwrap();
        let fixture = Fixture::new(&controller).await;
        let environment_id = fixture.create("environment_a", 1_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        let failed = fixture.read(&environment_id).await;
        assert_eq!(failed.state, EnvironmentState::Failed);
        assert_eq!(
            failed.state_message.as_deref(),
            Some(
                format!(
                    "the environment's envd speaks protocol version {}; this runtime requires {ENVD_PROTOCOL_VERSION}",
                    ENVD_PROTOCOL_VERSION + 1
                )
                .as_str()
            )
        );
        assert_eq!(
            failed.daemon_build.map(|build| build.protocol_version),
            Some(ENVD_PROTOCOL_VERSION + 1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unknown_template_is_a_refusal_that_fails_the_row() {
        let controller = FakeController::start().await.unwrap();
        let fixture = Fixture::new(&controller).await;
        let environment_id = EnvironmentId::new("environment_a");
        fixture
            .store
            .create_environment(CreateEnvironment {
                request_id: EnvironmentProvisionRequestId::new("request"),
                environment_id: environment_id.clone(),
                provider_id: fixture.provider_id.clone(),
                template_id: EnvironmentTemplateId::new("missing-template"),
                idle: None,
                display_name: None,
                metadata: BTreeMap::new(),
                created_at_ms: 1_000,
            })
            .await
            .unwrap();
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        assert_eq!(
            fixture.read(&environment_id).await.state,
            EnvironmentState::Failed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_runs_until_the_target_is_gone_and_revokes_routes() {
        let controller = FakeController::start().await.unwrap();
        let fixture = Fixture::new(&controller).await;
        let environment_id = fixture.create("environment_a", 1_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        let mut route = fixture
            .signals
            .watch(fixture.store.universe_id(), environment_id.as_str());
        fixture
            .store
            .begin_close_environment(BeginCloseEnvironment {
                environment_id: environment_id.clone(),
                reason: EnvironmentCloseReason::Requested,
                at_ms: 3_000,
            })
            .await
            .unwrap();
        // While the provider is down the close backs off and stays closing.
        controller.set_reachable(false);
        fixture
            .reconciler()
            .run_work(i64::MAX, 3_000)
            .await
            .unwrap();
        let closing = fixture.read(&environment_id).await;
        assert_eq!(closing.state, EnvironmentState::Closing);
        assert_eq!(closing.provisioned().unwrap().attempts, 1);
        controller.set_reachable(true);
        fixture
            .reconciler()
            .run_work(i64::MAX, 5_000)
            .await
            .unwrap();
        let closed = fixture.read(&environment_id).await;
        assert_eq!(closed.state, EnvironmentState::Closed);
        assert_eq!(closed.close_reason, Some(EnvironmentCloseReason::Requested));
        assert_eq!(controller.target_count().await, 0);
        assert!(route.has_changed().unwrap());
        route.mark_unchanged();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_display_sweep_records_power_and_ends_a_target_the_provider_removed() {
        let controller = FakeController::start().await.unwrap();
        let fixture = Fixture::new(&controller).await;
        let paused = fixture.create("environment_a", 1_000).await;
        let ended = fixture.create("environment_b", 1_000).await;
        fixture
            .reconciler()
            .run_work(i64::MAX, 2_000)
            .await
            .unwrap();
        assert!(
            controller
                .set_power_out_of_band(paused.as_str(), PowerState::Paused)
                .await
        );
        assert!(controller.end_target(ended.as_str()).await);
        fixture
            .reconciler()
            .run_display_sweep(10_000)
            .await
            .unwrap();
        let observed = fixture.read(&paused).await;
        assert_eq!(observed.state, EnvironmentState::Open);
        assert_eq!(
            observed.display_status(),
            ::environments::EnvironmentDisplayStatus::Paused
        );
        let gone = fixture.read(&ended).await;
        assert_eq!(gone.state, EnvironmentState::Closed);
        assert_eq!(
            gone.close_reason,
            Some(EnvironmentCloseReason::ProviderEnded)
        );
        // The sweep never powers anything: the paused target stays paused.
        assert_eq!(controller.target_count().await, 1);
        fixture
            .reconciler()
            .run_display_sweep(20_000)
            .await
            .unwrap();
        assert_eq!(
            fixture.read(&paused).await.display_status(),
            ::environments::EnvironmentDisplayStatus::Paused
        );
    }
}
