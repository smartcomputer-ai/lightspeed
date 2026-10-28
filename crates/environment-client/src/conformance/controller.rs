//! Controller-plane conformance: drives any controller endpoint through
//! [`ControllerClient`] and checks the contract every provider shares.
//!
//! The suite checks that:
//!
//! - `initialize` names the `controller` plane, refuses a wrong
//!   `protocolVersion` with `versionMismatch` (leaving the connection open
//!   and uninitialized), refuses requests before it with `notInitialized`,
//!   and refuses a repeat with `invalidRequest`;
//! - when the suite is given a bearer credential, a missing or wrong one is
//!   refused with HTTP 401 before the upgrade (a provider registered
//!   without a credential is not checked for one);
//! - `target/get` never creates and answers `notFound` for an unknown
//!   owner or id; a foreign owner on a known `targetId` is
//!   `failedPrecondition` (`ownerMismatch`);
//! - `target/create` answers promptly and is idempotent per owner;
//! - `target/close` by owner without a `targetId` closes, is idempotent
//!   (a repeat answers `closed`), and answers `closed` without `targetId`
//!   for an owner with nothing;
//! - optional methods (`target/setPower` without `capabilities.power`,
//!   `ingress/*` without `capabilities.ingress`) answer
//!   `capabilityUnavailable`;
//! - when the suite is given the provider's data route base, the created
//!   target's data route ([`provider_data_route`]) upgrades, refusing at
//!   most `waking` or `booting` with `Retry-After` on the way; with
//!   `capabilities.power` and a template that lists a powered-down state,
//!   a target moved there by `target/setPower` wakes on the next dial
//!   (obligation 2); and once the target is closed the route refuses
//!   410 `gone`.
//!
//! Every answer is decoded strictly, so an observation that carries a
//! member this protocol version does not define fails the check that read
//! it. Owners are `{universeId, environmentId}`.
//!
//! The suite creates one target from the configured template and closes it
//! again; every other check uses fresh owners that hold nothing. The data
//! route checks are built on [`data_route`](super::data_route).
//!
//! ```no_run
//! # async fn example(options: environment_client::ConnectOptions,
//! #     template_id: environment_protocol::ids::TemplateId,
//! #     owner: std::sync::Arc<dyn Fn() -> environment_protocol::control::target::TargetOwner + Send + Sync>) {
//! use environment_client::conformance::controller::{ControllerConformance, run};
//!
//! let suite = ControllerConformance::new("wss://provider.example/control", options, template_id, owner);
//! run(&suite).await.into_result().expect("controller conforms");
//! # }
//! ```

use std::{
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION,
    bytes::SecretString,
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            CloseTargetParams, CreateTargetParams, GetTargetParams, PowerState,
            SetTargetPowerParams, TargetObservation, TargetOwner, TargetPhase, TargetRef,
        },
        template::ListTemplatesParams,
    },
    error::{Capability, ErrorData, PreconditionReason, ProtocolErrorKind},
    handshake::Plane,
    ids::{ProviderTargetId, TemplateId},
    manifest::{ControllerInitialize, TargetGet},
    routes::{DataRouteBase, DataRouteRefusal, provider_data_route},
};
use thiserror::Error;

use crate::{
    ClientError, ConnectOptions, ControllerClient,
    conformance::data_route::{await_upgrade, expect_refusal},
    connection::Connection,
    controller::initialize,
};

/// What to run the suite against.
#[derive(Clone)]
pub struct ControllerConformance {
    /// The controller URL, as registered for the provider.
    pub endpoint: String,
    /// Connect options carrying the provider credential, when the provider
    /// is registered with one, as `bearer_token`.
    pub options: ConnectOptions,
    /// A template the suite may create one target from.
    pub template_id: TemplateId,
    /// Returns a fresh owner on every call, in a universe the provider
    /// serves and never used before.
    pub owner: Arc<dyn Fn() -> TargetOwner + Send + Sync>,
    /// The provider's data route base, as Lightspeed dials it: configured,
    /// or [`DataRouteBase::derived_from_controller`]. When set, the suite
    /// also dials the created target's data route with `options`. Default
    /// none.
    pub data_route_base: Option<DataRouteBase>,
    /// Bound on every answer: target methods answer promptly and continue
    /// slow work in the background. Default 15 s.
    pub prompt_bound: Duration,
    /// How long the created target may take to carry a `targetId`, and its
    /// close to reach `closed`. Default 10 min.
    pub settle_timeout: Duration,
    /// Pause between observations while waiting. Default 2 s.
    pub poll_interval: Duration,
}

impl ControllerConformance {
    pub fn new(
        endpoint: impl Into<String>,
        options: ConnectOptions,
        template_id: TemplateId,
        owner: Arc<dyn Fn() -> TargetOwner + Send + Sync>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            options,
            template_id,
            owner,
            data_route_base: None,
            prompt_bound: Duration::from_secs(15),
            settle_timeout: Duration::from_secs(600),
            poll_interval: Duration::from_secs(2),
        }
    }
}

impl fmt::Debug for ControllerConformance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerConformance")
            .field("endpoint", &self.endpoint)
            .field("template_id", &self.template_id)
            .field("data_route_base", &self.data_route_base)
            .field("prompt_bound", &self.prompt_bound)
            .field("settle_timeout", &self.settle_timeout)
            .finish_non_exhaustive()
    }
}

/// The outcome of one check.
#[derive(Clone, Debug)]
pub struct CheckResult {
    /// Stable check name, such as `target.get.foreignOwner`.
    pub name: &'static str,
    /// Why the check failed.
    pub failure: Option<String>,
}

/// Every check that ran, in order.
#[derive(Clone, Debug, Default)]
pub struct ConformanceReport {
    pub checks: Vec<CheckResult>,
}

impl ConformanceReport {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.failure.is_none())
    }

    pub fn failures(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|check| check.failure.is_some())
    }

    /// `Ok` when every check passed.
    pub fn into_result(self) -> Result<Self, ConformanceFailure> {
        if self.passed() {
            return Ok(self);
        }
        Err(ConformanceFailure {
            failures: self
                .failures()
                .map(|check| (check.name, check.failure.clone().unwrap_or_default()))
                .collect(),
        })
    }

    fn record(&mut self, name: &'static str, outcome: Result<(), String>) -> bool {
        let passed = outcome.is_ok();
        self.checks.push(CheckResult {
            name,
            failure: outcome.err(),
        });
        passed
    }
}

/// The failed checks of a run.
#[derive(Clone, Debug, Error)]
#[error("controller conformance failed: {}", describe(failures))]
pub struct ConformanceFailure {
    /// Check name and why it failed.
    pub failures: Vec<(&'static str, String)>,
}

fn describe(failures: &[(&'static str, String)]) -> String {
    failures
        .iter()
        .map(|(name, failure)| format!("{name}: {failure}"))
        .collect::<Vec<_>>()
        .join("; ")
}

type Check = Result<(), String>;

/// Run every check against `suite.endpoint`. Checks that need the
/// handshake are skipped (and reported) when it fails.
pub async fn run(suite: &ControllerConformance) -> ConformanceReport {
    let mut report = ConformanceReport::default();
    let connected = ControllerClient::connect_initialized(
        &suite.endpoint,
        with_timeout(suite.options.clone(), suite.prompt_bound),
    )
    .await;
    let (client, initialize) = match connected {
        Ok(connected) => connected,
        Err(error) => {
            report.record(
                "initialize",
                Err(format!("connect_initialized failed: {error}")),
            );
            return report;
        }
    };
    report.record("initialize", check_initialize(&initialize));
    report.record(
        "initialize.repeat",
        check_repeat_initialize(suite, &client).await,
    );
    report.record("initialize.order", check_initialize_order(suite).await);
    if suite.options.bearer_token.is_some() {
        report.record("credential", check_credential(suite).await);
    }
    report.record(
        "target.get.unknown",
        check_get_unknown(suite, &client).await,
    );
    report.record(
        "target.close.nothing",
        check_close_nothing(suite, &client).await,
    );
    report.record(
        "capabilities.optional",
        check_optional_methods(suite, &client, &initialize).await,
    );

    let owner = (suite.owner)();
    let created = check_create(suite, &client, &owner).await;
    let created_target = created.as_ref().ok().cloned().flatten();
    report.record("target.create", created.map(|_| ()));
    let route = created_target.as_ref().and_then(|target_id| {
        suite
            .data_route_base
            .as_ref()
            .map(|base| provider_data_route(base, &owner, target_id))
    });
    if let Some(target_id) = &created_target {
        report.record(
            "target.get.foreignOwner",
            check_foreign_owner(suite, &client, target_id).await,
        );
    }
    if let (Some(target_id), Some(route)) = (&created_target, &route) {
        let upgraded = report.record("dataRoute.upgrade", check_upgrade(suite, route).await);
        if upgraded && initialize.capabilities.power {
            match power_down_state(suite, &client, &owner).await {
                Ok(Some(power)) => {
                    report.record(
                        "dataRoute.wake",
                        check_wake(suite, &client, &owner, target_id, route, power).await,
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    report.record("dataRoute.wake", Err(error));
                }
            }
        }
    }
    let closed = report.record(
        "target.close.byOwner",
        check_close_by_owner(suite, &client, &owner).await,
    );
    if closed && let Some(route) = &route {
        report.record("dataRoute.gone", check_gone(suite, route).await);
    }
    let _ = client.close().await;
    report
}

fn with_timeout(mut options: ConnectOptions, bound: Duration) -> ConnectOptions {
    options.request_timeout = Some(bound);
    options
}

fn check_initialize(initialize: &ControllerInitializeResponse) -> Check {
    if initialize.plane != Plane::Controller {
        return Err(format!("plane is {}", initialize.plane));
    }
    if initialize.protocol_version != CONTROLLER_PROTOCOL_VERSION {
        return Err(format!(
            "protocolVersion is {}",
            initialize.protocol_version
        ));
    }
    Ok(())
}

async fn check_repeat_initialize(
    suite: &ControllerConformance,
    client: &ControllerClient,
) -> Check {
    let repeated = client
        .call::<ControllerInitialize>(&ControllerInitializeParams {
            protocol_version: CONTROLLER_PROTOCOL_VERSION,
            client_info: suite.options.client_info.clone(),
        })
        .await;
    expect_kind(repeated.map(|_| ()), ProtocolErrorKind::InvalidRequest)
}

/// `notInitialized` before `initialize`; `versionMismatch` for another
/// version, after which the same connection still initializes.
async fn check_initialize_order(suite: &ControllerConformance) -> Check {
    let options = with_timeout(suite.options.clone(), suite.prompt_bound);
    let connection = Connection::connect(&suite.endpoint, &options, Plane::Controller)
        .await
        .map_err(|error| format!("connect: {error}"))?;
    let outcome = async {
        let early = connection
            .call::<TargetGet>(
                &GetTargetParams {
                    target: TargetRef {
                        owner: (suite.owner)(),
                        target_id: None,
                    },
                },
                None,
            )
            .await;
        expect_kind(early.map(|_| ()), ProtocolErrorKind::NotInitialized)
            .map_err(|error| format!("target/get before initialize: {error}"))?;

        let wrong = CONTROLLER_PROTOCOL_VERSION + 1;
        let params = serde_json::to_value(ControllerInitializeParams {
            protocol_version: wrong,
            client_info: suite.options.client_info.clone(),
        })
        .map_err(|error| error.to_string())?;
        let response = connection
            .request(ControllerInitialize::NAME, params, Some(suite.prompt_bound))
            .await
            .map_err(|error| format!("initialize with version {wrong}: {error}"))?;
        match response.result {
            Err(error) => match error.data {
                ErrorData::VersionMismatch { expected, received }
                    if expected == CONTROLLER_PROTOCOL_VERSION && received == wrong => {}
                data => {
                    return Err(format!(
                        "initialize with version {wrong} answered {data:?}, expected \
                         versionMismatch {{expected: {CONTROLLER_PROTOCOL_VERSION}, received: {wrong}}}"
                    ));
                }
            },
            Ok(_) => return Err(format!("initialize with version {wrong} succeeded")),
        }
        initialize(&connection, &options)
            .await
            .map(|_| ())
            .map_err(|error| format!("initialize after a mismatch: {error}"))
    }
    .await;
    let _ = connection.close().await;
    outcome
}

async fn check_credential(suite: &ControllerConformance) -> Check {
    let mut missing = suite.options.clone();
    missing.bearer_token = None;
    let mut wrong = suite.options.clone();
    wrong.bearer_token = Some(SecretString::new("lightspeed-conformance-wrong-credential"));
    for (label, options) in [("missing", missing), ("wrong", wrong)] {
        match ControllerClient::connect_initialized(&suite.endpoint, options).await {
            Err(ClientError::UpgradeRejected { status: 401, .. }) => {}
            Err(error) => {
                return Err(format!(
                    "{label} credential: expected HTTP 401, got {error}"
                ));
            }
            Ok((client, _)) => {
                let _ = client.close().await;
                return Err(format!("{label} credential was accepted"));
            }
        }
    }
    Ok(())
}

async fn check_get_unknown(suite: &ControllerConformance, client: &ControllerClient) -> Check {
    let owner = (suite.owner)();
    let by_owner = get(client, &owner, None).await;
    expect_kind(by_owner.map(|_| ()), ProtocolErrorKind::NotFound)
        .map_err(|error| format!("target/get of an owner with nothing: {error}"))?;
    let unknown = ProviderTargetId::new("lightspeed-conformance-unknown-target")
        .map_err(|error| error.to_string())?;
    let by_id = get(client, &owner, Some(unknown)).await;
    expect_kind(by_id.map(|_| ()), ProtocolErrorKind::NotFound)
        .map_err(|error| format!("target/get of an unknown targetId: {error}"))?;
    let again = get(client, &owner, None).await;
    expect_kind(again.map(|_| ()), ProtocolErrorKind::NotFound)
        .map_err(|error| format!("target/get created a target: {error}"))
}

async fn check_close_nothing(suite: &ControllerConformance, client: &ControllerClient) -> Check {
    let owner = (suite.owner)();
    let observation = close(client, &owner)
        .await
        .map_err(|error| format!("target/close of an owner with nothing: {error}"))?;
    if observation.phase != TargetPhase::Closed || observation.target_id.is_some() {
        return Err(format!(
            "target/close of an owner with nothing answered phase {:?} with targetId {:?}; \
             expected closed without targetId",
            observation.phase, observation.target_id
        ));
    }
    check_owner_echo(&observation, &owner)
}

async fn check_optional_methods(
    suite: &ControllerConformance,
    client: &ControllerClient,
    initialize: &ControllerInitializeResponse,
) -> Check {
    if !initialize.capabilities.power {
        let powered = client
            .set_target_power(&SetTargetPowerParams {
                target: TargetRef {
                    owner: (suite.owner)(),
                    target_id: None,
                },
                power: PowerState::Running,
            })
            .await;
        expect_capability(powered.map(|_| ()), Capability::Power)
            .map_err(|error| format!("target/setPower without power: {error}"))?;
    }
    if !initialize.capabilities.ingress {
        let target = TargetRef {
            owner: (suite.owner)(),
            target_id: None,
        };
        let ensured = client
            .ensure_ingress(&EnsureIngressParams {
                target: target.clone(),
            })
            .await;
        expect_capability(ensured.map(|_| ()), Capability::Ingress)
            .map_err(|error| format!("ingress/ensure without ingress: {error}"))?;
        let removed = client.remove_ingress(&RemoveIngressParams { target }).await;
        expect_capability(removed.map(|_| ()), Capability::Ingress)
            .map_err(|error| format!("ingress/remove without ingress: {error}"))?;
    }
    Ok(())
}

/// Create twice for one owner, each answered within the prompt bound, then
/// wait for the target's id. Returns that id.
async fn check_create(
    suite: &ControllerConformance,
    client: &ControllerClient,
    owner: &TargetOwner,
) -> Result<Option<ProviderTargetId>, String> {
    let params = CreateTargetParams {
        owner: owner.clone(),
        template_id: suite.template_id.clone(),
        display_name: None,
        idle: None,
    };
    let first = timed(suite, "target/create", client.create_target(&params)).await?;
    let second = timed(
        suite,
        "repeated target/create",
        client.create_target(&params),
    )
    .await?;
    check_owner_echo(&first.target, owner)?;
    check_owner_echo(&second.target, owner)?;
    if let (Some(first_id), Some(second_id)) = (&first.target.target_id, &second.target.target_id)
        && first_id != second_id
    {
        return Err(format!(
            "target/create is not idempotent per owner: {first_id} then {second_id}"
        ));
    }
    let mut target_id = second.target.target_id.or(first.target.target_id);
    let deadline = Instant::now() + suite.settle_timeout;
    while target_id.is_none() {
        if Instant::now() >= deadline {
            return Err(format!(
                "the target carried no targetId within {:?}",
                suite.settle_timeout
            ));
        }
        tokio::time::sleep(suite.poll_interval).await;
        let observation = get(client, owner, None)
            .await
            .map_err(|error| format!("target/get of the created target: {error}"))?;
        target_id = observation.target_id;
    }
    let repeated = timed(
        suite,
        "target/create after the id",
        client.create_target(&params),
    )
    .await?;
    if repeated.target.target_id != target_id {
        return Err(format!(
            "target/create after the id answered targetId {:?}, expected {:?}",
            repeated.target.target_id, target_id
        ));
    }
    Ok(target_id)
}

async fn check_foreign_owner(
    suite: &ControllerConformance,
    client: &ControllerClient,
    target_id: &ProviderTargetId,
) -> Check {
    let foreign = (suite.owner)();
    match get(client, &foreign, Some(target_id.clone())).await {
        Err(ClientError::Protocol(error))
            if matches!(
                error.data,
                ErrorData::FailedPrecondition {
                    reason: PreconditionReason::OwnerMismatch
                }
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(format!(
            "expected failedPrecondition (ownerMismatch), got {error}"
        )),
        Ok(observation) => Err(format!(
            "a foreign owner observed the target (phase {:?})",
            observation.phase
        )),
    }
}

/// Close by owner without a `targetId` until `closed`, then once more.
async fn check_close_by_owner(
    suite: &ControllerConformance,
    client: &ControllerClient,
    owner: &TargetOwner,
) -> Check {
    let deadline = Instant::now() + suite.settle_timeout;
    loop {
        let observation = timed(suite, "target/close", async {
            client
                .close_target(&CloseTargetParams {
                    target: TargetRef {
                        owner: owner.clone(),
                        target_id: None,
                    },
                })
                .await
        })
        .await?
        .target;
        check_owner_echo(&observation, owner)?;
        match observation.phase {
            TargetPhase::Closed => break,
            TargetPhase::Closing => {}
            phase => return Err(format!("target/close answered phase {phase:?}")),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the target did not reach closed within {:?}",
                suite.settle_timeout
            ));
        }
        tokio::time::sleep(suite.poll_interval).await;
    }
    let again = close(client, owner)
        .await
        .map_err(|error| format!("repeated target/close: {error}"))?;
    if again.phase != TargetPhase::Closed {
        return Err(format!(
            "repeated target/close answered phase {:?}",
            again.phase
        ));
    }
    Ok(())
}

/// The created target's data route upgrades within the settle timeout,
/// refusing at most `waking` or `booting` on the way.
async fn check_upgrade(suite: &ControllerConformance, route: &str) -> Check {
    let refusals = await_upgrade(route, suite.options.clone(), suite.settle_timeout).await?;
    check_wait_refusals(&refusals)
}

/// The first powered-down state the configured template lists for
/// `target/setPower`, if any.
async fn power_down_state(
    suite: &ControllerConformance,
    client: &ControllerClient,
    owner: &TargetOwner,
) -> Result<Option<PowerState>, String> {
    let templates = timed(
        suite,
        "template/list",
        client.list_templates(&ListTemplatesParams {
            universe_id: owner.universe_id.clone(),
        }),
    )
    .await?;
    let template = templates
        .templates
        .iter()
        .find(|template| template.template_id == suite.template_id)
        .ok_or_else(|| format!("template/list does not list {}", suite.template_id))?;
    Ok([
        PowerState::Paused,
        PowerState::Suspended,
        PowerState::Stopped,
    ]
    .into_iter()
    .find(|power| template.power_states.contains(power)))
}

/// Move the target to `power`, wait until it is observed there, then dial
/// its data route: the dial wakes it and upgrades within the settle
/// timeout, refusing at most `waking` or `booting` on the way.
async fn check_wake(
    suite: &ControllerConformance,
    client: &ControllerClient,
    owner: &TargetOwner,
    target_id: &ProviderTargetId,
    route: &str,
    power: PowerState,
) -> Check {
    let target = TargetRef {
        owner: owner.clone(),
        target_id: Some(target_id.clone()),
    };
    timed(
        suite,
        "target/setPower",
        client.set_target_power(&SetTargetPowerParams {
            target: target.clone(),
            power,
        }),
    )
    .await?;
    let deadline = Instant::now() + suite.settle_timeout;
    loop {
        let observation = get(client, owner, Some(target_id.clone()))
            .await
            .map_err(|error| format!("target/get after target/setPower {power}: {error}"))?;
        if observation.power == Some(power) {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the target was not observed {power} within {:?} (last {:?})",
                suite.settle_timeout, observation.power
            ));
        }
        tokio::time::sleep(suite.poll_interval).await;
    }
    let refusals = await_upgrade(route, suite.options.clone(), suite.settle_timeout)
        .await
        .map_err(|error| format!("dialing the {power} target: {error}"))?;
    check_wait_refusals(&refusals)
}

/// Once the target is closed its data route refuses 410 `gone`.
async fn check_gone(suite: &ControllerConformance, route: &str) -> Check {
    match expect_refusal(route, suite.options.clone()).await? {
        DataRouteRefusal::Gone {} => Ok(()),
        refusal => Err(format!(
            "the closed target's data route refused {refusal}, expected gone"
        )),
    }
}

/// A provider data route waits only with `waking` or `booting`; the other
/// 503 refusals are the gateway's.
fn check_wait_refusals(refusals: &[DataRouteRefusal]) -> Check {
    match refusals.iter().find(|refusal| {
        !matches!(
            refusal,
            DataRouteRefusal::Waking {} | DataRouteRefusal::Booting {}
        )
    }) {
        Some(refusal) => Err(format!(
            "the provider data route refused {refusal}; a provider waits only with waking or booting"
        )),
        None => Ok(()),
    }
}

async fn get(
    client: &ControllerClient,
    owner: &TargetOwner,
    target_id: Option<ProviderTargetId>,
) -> Result<TargetObservation, ClientError> {
    client
        .get_target(&GetTargetParams {
            target: TargetRef {
                owner: owner.clone(),
                target_id,
            },
        })
        .await
        .map(|response| response.target)
}

async fn close(
    client: &ControllerClient,
    owner: &TargetOwner,
) -> Result<TargetObservation, ClientError> {
    client
        .close_target(&CloseTargetParams {
            target: TargetRef {
                owner: owner.clone(),
                target_id: None,
            },
        })
        .await
        .map(|response| response.target)
}

/// Await `call` and require it to answer within the prompt bound.
async fn timed<T>(
    suite: &ControllerConformance,
    label: &str,
    call: impl Future<Output = Result<T, ClientError>>,
) -> Result<T, String> {
    let started = Instant::now();
    let outcome = call.await.map_err(|error| format!("{label}: {error}"))?;
    let elapsed = started.elapsed();
    if elapsed > suite.prompt_bound {
        return Err(format!(
            "{label} took {elapsed:?}, over the {:?} prompt bound",
            suite.prompt_bound
        ));
    }
    Ok(outcome)
}

fn check_owner_echo(observation: &TargetObservation, owner: &TargetOwner) -> Check {
    if &observation.owner != owner {
        return Err(format!(
            "the observation echoes owner {:?}, expected {:?}",
            observation.owner, owner
        ));
    }
    Ok(())
}

fn expect_kind(outcome: Result<(), ClientError>, kind: ProtocolErrorKind) -> Check {
    match outcome {
        Err(ClientError::Protocol(error)) if error.kind() == kind => Ok(()),
        Err(error) => Err(format!("expected {kind}, got {error}")),
        Ok(()) => Err(format!("expected {kind}, the call succeeded")),
    }
}

fn expect_capability(outcome: Result<(), ClientError>, capability: Capability) -> Check {
    match outcome {
        Err(ClientError::Protocol(error))
            if error.data == (ErrorData::CapabilityUnavailable { capability }) =>
        {
            Ok(())
        }
        Err(error) => Err(format!(
            "expected capabilityUnavailable ({capability:?}), got {error}"
        )),
        Ok(()) => Err(format!(
            "expected capabilityUnavailable ({capability:?}), the call succeeded"
        )),
    }
}
