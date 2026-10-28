//! The provider's listener: the controller plane on `/control` and the data
//! routes under `/routes`, both behind the provider credential when one is
//! configured.
//!
//! Every target method answers promptly with the current observation. Slow
//! Incus work (an image pull, a boot, a graceful shutdown) runs as one
//! background job per target, serialized per target, and later calls observe
//! its progress: a running create reports `provisioning`, a running close
//! `closing`, and a failed create `failed` with its error as the `message`.
//! Job state is in memory; Incus itself, read through the deterministic owner
//! name, is the record of every target.
//!
//! The provider owns its targets' power. `target/setPower` serves manual
//! changes, the idle loop ([`crate::idle`]) powers idle targets down and
//! closes them on its own clock, and a data route dial wakes a powered-down
//! target before it relays.

use std::{
    collections::{BTreeMap, HashMap},
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION,
    control::{
        handshake::{
            ControllerCapabilities, ControllerInitializeParams, ControllerInitializeResponse,
        },
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            CloseTargetParams, CreateTargetParams, GetTargetParams, IngressObservation, PowerState,
            SetTargetPowerParams, TargetObservation, TargetOwner, TargetPhase, TargetRef,
            TargetResponse,
        },
        template::{EnvironmentTemplate, ListTemplatesParams, ListTemplatesResponse},
    },
    error::{BusyReason, Capability, PreconditionReason, ProtocolError},
    handshake::{ImplementationInfo, Plane},
    idle::IdleAction,
    ids::{EnvironmentId, ProviderTargetId, TemplateId, UniverseId},
    jsonrpc::{self, Request, Response as RpcResponse},
    limits::{MAX_DATA_ROUTE_HOLD_MS, MAX_MESSAGE_BYTES, RETRY_AFTER_MAX_SECS},
    manifest::{
        self, ControllerInitialize, IngressEnsure, IngressRemove, MessageKind, TargetClose,
        TargetCreate, TargetGet, TargetSetPower, TemplateList,
    },
    metadata::{DescriptiveText, Metadata, validate_metadata_entry},
    routes::DataRouteRefusal,
};
use futures_util::{FutureExt as _, SinkExt as _, StreamExt as _, future::BoxFuture};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, Semaphore, mpsc, watch};

use crate::{
    Config, IncusBackend, idle,
    incus::{InstanceStatus, OwnedTarget, SUPPORTED_POWER_STATES, unsupported_idle_stage},
    ingress, policy,
    relay::{self, GuestSocket, Relays},
};

/// How long a mutation waits for its background job before it answers with
/// the current observation.
const PROMPT_WAIT: Duration = Duration::from_secs(5);

/// Requests one controller connection may have in flight before `busy`.
const MAX_CONCURRENT_REQUESTS: usize = 32;

const _: () = assert!(
    MAX_CONCURRENT_REQUESTS >= environment_protocol::limits::MIN_CONCURRENT_REQUESTS as usize
);

/// `Retry-After` of a data route's 503 refusal: about one guest boot step.
const RETRY_AFTER_SECS: u64 = 2;

const _: () = assert!(RETRY_AFTER_SECS >= 1 && RETRY_AFTER_SECS <= RETRY_AFTER_MAX_SECS);

/// Bound on one guest dial attempt while a data route dial waits, so the
/// wait re-reads the instance between attempts.
const DIAL_ATTEMPT: Duration = Duration::from_secs(1);

/// Pause between two looks at the instance while a data route dial waits.
const HOLD_POLL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(crate) struct App<B> {
    pub(crate) config: Config,
    pub(crate) backend: B,
    pub(crate) work: Arc<Work>,
    pub(crate) relays: Relays,
    /// When data route dials last woke instances.
    pub(crate) wakes: idle::Wakes,
    initialize: Arc<ControllerInitializeResponse>,
    /// How long a data route dial may wait for its target before it refuses.
    data_route_hold: Duration,
}

impl<B: IncusBackend> App<B> {
    pub(crate) fn new(config: Config, backend: B) -> Self {
        let initialize = ControllerInitializeResponse {
            protocol_version: CONTROLLER_PROTOCOL_VERSION,
            plane: Plane::Controller,
            server_info: server_info(),
            capabilities: ControllerCapabilities {
                power: true,
                ingress: config.ingress.is_some(),
            },
        };
        let wakes = idle::Wakes::new(idle::wake_grace(config.idle_check_seconds));
        Self {
            config,
            backend,
            work: Arc::default(),
            relays: Relays::default(),
            wakes,
            initialize: Arc::new(initialize),
            data_route_hold: Duration::from_millis(MAX_DATA_ROUTE_HOLD_MS),
        }
    }
}

pub(crate) fn server_info() -> ImplementationInfo {
    let info = ImplementationInfo::new("lightspeed-provider-incus")
        .expect("the provider name is a valid implementation name");
    let info = info
        .clone()
        .with_version(release_info::VERSION)
        .unwrap_or(info);
    let info = info
        .clone()
        .with_git_sha(release_info::GIT_SHA)
        .unwrap_or(info);
    info.clone()
        .with_target(release_info::TARGET)
        .unwrap_or(info)
}

pub async fn serve<B: IncusBackend>(config: Config, backend: B) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.controller_listen).await?;
    serve_on(listener, config, backend).await
}

/// Serve on an already bound listener, with the idle loop running beside
/// it.
pub async fn serve_on<B: IncusBackend>(
    listener: tokio::net::TcpListener,
    config: Config,
    backend: B,
) -> anyhow::Result<()> {
    let app = App::new(config, backend);
    let idle_loop = tokio::spawn(idle::run(app.clone()));
    let served = axum::serve(listener, router(app)).await;
    idle_loop.abort();
    served?;
    Ok(())
}

fn router<B: IncusBackend>(app: App<B>) -> Router {
    Router::new()
        .route("/health", get(health::<B>))
        .route("/control", get(control_upgrade::<B>))
        .route(
            "/routes/{universe}/{environment}/{target}",
            get(data_upgrade::<B>),
        )
        .with_state(app)
}

async fn health<B: IncusBackend>(State(app): State<App<B>>) -> Response {
    match app.backend.topology().await {
        Ok(topology) => Json(topology).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// True when no credential is configured, or when the request carries it as
/// a bearer token.
fn authorized(config: &Config, headers: &HeaderMap) -> bool {
    let Some(credential) = &config.credential else {
        return true;
    };
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| credential.matches(token))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
    )
        .into_response()
}

fn with_limits(upgrade: WebSocketUpgrade) -> WebSocketUpgrade {
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES as usize)
        .max_frame_size(MAX_MESSAGE_BYTES as usize)
}

async fn control_upgrade<B: IncusBackend>(
    State(app): State<App<B>>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !authorized(&app.config, &headers) {
        return unauthorized();
    }
    match upgrade {
        Ok(upgrade) => with_limits(upgrade)
            .on_upgrade(move |socket| connection(app, socket))
            .into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

/// A target's data route. A powered-down target is woken first; the dial
/// then waits, at most the protocol's hold bound, until envd answers, and
/// otherwise refuses with a typed [`DataRouteRefusal`]. The whole dial and
/// the relay after it hold the instance's [`RelayHold`](relay::RelayHold),
/// so the idle loop leaves it alone.
async fn data_upgrade<B: IncusBackend>(
    State(app): State<App<B>>,
    Path((universe, environment, target_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !authorized(&app.config, &headers) {
        return unauthorized();
    }
    let owner = match (UniverseId::new(universe), EnvironmentId::new(environment)) {
        (Ok(universe_id), Ok(environment_id)) => TargetOwner {
            universe_id,
            environment_id,
        },
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    // Any other id names no target this owner can hold.
    if target_id != policy::instance_name(&owner) {
        return refuse(DataRouteRefusal::Gone {});
    }
    let hold = app.relays.hold(&target_id);
    let mut woke = false;
    let dialed = tokio::time::timeout(
        app.data_route_hold,
        wake_and_dial(&app, &owner, &target_id, &mut woke),
    )
    .await;
    // A woken instance gets the wake grace once this connection ends, so
    // the caller's next dial, or its first `use`, arrives before the idle
    // loop looks again.
    if woke && !matches!(dialed, Ok(Ok(_))) {
        app.wakes.record(&target_id, Instant::now());
    }
    match dialed {
        Ok(Ok(guest)) => {
            let config = app.config.clone();
            let wakes = woke.then(|| app.wakes.clone());
            with_limits(upgrade)
                .on_upgrade(move |socket| async move {
                    let _hold = hold;
                    relay::proxy(config, socket, guest).await;
                    if let Some(wakes) = wakes {
                        wakes.record(&target_id, Instant::now());
                    }
                })
                .into_response()
        }
        Ok(Err(DialRefusal::Refused(refusal))) => refuse(refusal),
        Ok(Err(DialRefusal::Backend)) => StatusCode::BAD_GATEWAY.into_response(),
        Err(_) if woke => refuse(DataRouteRefusal::Waking {}),
        Err(_) => refuse(DataRouteRefusal::Booting {}),
    }
}

/// Why a data route dial does not upgrade.
enum DialRefusal {
    Refused(DataRouteRefusal),
    /// Incus could not be read; answered 502 without a body.
    Backend,
}

/// Wake the owner's instance when it is powered down, then dial its envd
/// until it answers. Runs until it succeeds or meets a terminal state; the
/// caller bounds it. `woke` records that this dial started a wake.
async fn wake_and_dial<B: IncusBackend>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    woke: &mut bool,
) -> Result<GuestSocket, DialRefusal> {
    loop {
        let job = app
            .work
            .view(name)
            .filter(Slot::running)
            .map(|slot| slot.kind);
        if job == Some(WorkKind::Close) {
            return Err(DialRefusal::Refused(DataRouteRefusal::Gone {}));
        }
        // A wake runs only after the power-down it superseded, which the
        // slot no longer shows: until the wake finished, the instance may
        // still read running while it is about to freeze or stop.
        if job == Some(WorkKind::Power(PowerState::Running)) {
            *woke = true;
            tokio::time::sleep(HOLD_POLL).await;
            continue;
        }
        let target = match app.backend.get_owned(&owner.universe_id, name).await {
            Ok(Some(target)) if target.owner == *owner => Some(target),
            // A create still running has no instance to show yet.
            Ok(_) if job == Some(WorkKind::Create) => None,
            Ok(_) => return Err(DialRefusal::Refused(DataRouteRefusal::Gone {})),
            Err(_) => return Err(DialRefusal::Backend),
        };
        // A create still running starts the instance itself.
        if let Some(target) = target.filter(|_| job != Some(WorkKind::Create)) {
            let powering_down =
                matches!(job, Some(WorkKind::Power(power)) if power != PowerState::Running);
            match target.status {
                InstanceStatus::Error => {
                    return Err(DialRefusal::Refused(DataRouteRefusal::Failed {
                        message: DescriptiveText::lossy(INSTANCE_ERROR)
                            .expect("the error text is descriptive"),
                    }));
                }
                InstanceStatus::Frozen | InstanceStatus::Stopped | InstanceStatus::Stopping => {
                    wake(app, owner, name);
                    *woke = true;
                }
                InstanceStatus::Running | InstanceStatus::Starting if powering_down => {
                    wake(app, owner, name);
                    *woke = true;
                }
                InstanceStatus::Running | InstanceStatus::Starting => {
                    if target.ipv4_address.is_some() {
                        let attempt =
                            DIAL_ATTEMPT.min(Duration::from_secs(app.config.dial_timeout_seconds));
                        if let Ok(guest) =
                            relay::dial_guest_within(&app.config, &target, attempt).await
                        {
                            return Ok(guest);
                        }
                    }
                }
                InstanceStatus::Other => {}
            }
        }
        tokio::time::sleep(HOLD_POLL).await;
    }
}

/// Power the instance up as its background job, unless a wake is already
/// running. A power-down job still running finishes first, then this one
/// runs; [`wake_and_dial`] dials only once it finished.
fn wake<B: IncusBackend>(app: &App<B>, owner: &TargetOwner, name: &str) {
    app.work.start(
        name,
        WorkKind::Power(PowerState::Running),
        power_job(&app.backend, &owner.universe_id, name, PowerState::Running),
    );
}

/// A data route refusal: its status, its JSON body and, for a 503,
/// `Retry-After`.
fn refuse(refusal: DataRouteRefusal) -> Response {
    let status = StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    let retryable = refusal.is_retryable();
    let mut response = (status, Json(refusal)).into_response();
    if retryable {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECS));
    }
    response
}

/// One controller connection: the JSON-RPC envelope, the `initialize`-first
/// handshake, and pipelined requests answered in any order by one writer.
async fn connection<B: IncusBackend>(app: App<B>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (outgoing, mut queue) = mpsc::channel::<Message>(MAX_CONCURRENT_REQUESTS * 2);
    let writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let mut initialized = false;
    while let Some(Ok(frame)) = stream.next().await {
        let reply = match frame {
            Message::Text(text) => match jsonrpc::parse(text.as_str()) {
                Err(rejection) => rejection.into_response(),
                Ok(jsonrpc::Message::Request(request)) => {
                    if request.method == ControllerInitialize::NAME {
                        Some(initialize(&app, &mut initialized, &request))
                    } else if !initialized {
                        Some(RpcResponse::failure(
                            Some(request.id),
                            ProtocolError::not_initialized(),
                        ))
                    } else {
                        match permits.clone().try_acquire_owned() {
                            Ok(permit) => {
                                let app = app.clone();
                                let outgoing = outgoing.clone();
                                tokio::spawn(async move {
                                    let response = handle(&app, request).await;
                                    let _ = outgoing
                                        .send(Message::Text(response.to_text().into()))
                                        .await;
                                    drop(permit);
                                });
                                None
                            }
                            Err(_) => Some(RpcResponse::failure(
                                Some(request.id),
                                ProtocolError::busy(
                                    Some(BusyReason::RequestLimit),
                                    None,
                                    format!(
                                        "more than {MAX_CONCURRENT_REQUESTS} requests in flight"
                                    ),
                                ),
                            )),
                        }
                    }
                }
                // The controller plane defines no notifications, and this
                // server sends no requests, so no response answers anything.
                Ok(jsonrpc::Message::Notification(_) | jsonrpc::Message::Response(_)) => None,
            },
            Message::Binary(_) => Some(RpcResponse::failure(
                None,
                ProtocolError::invalid_request("binary frames are not part of any plane"),
            )),
            Message::Ping(payload) => {
                if outgoing.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
                None
            }
            Message::Pong(_) => None,
            Message::Close(_) => break,
        };
        if let Some(reply) = reply
            && outgoing
                .send(Message::Text(reply.to_text().into()))
                .await
                .is_err()
        {
            break;
        }
    }
    drop(outgoing);
    let _ = writer.await;
}

fn initialize<B>(app: &App<B>, initialized: &mut bool, request: &Request) -> RpcResponse {
    let id = Some(request.id.clone());
    if *initialized {
        return RpcResponse::failure(
            id,
            ProtocolError::invalid_request("initialize was already called on this connection"),
        );
    }
    // The version decides before the rest of the params: a peer of another
    // version may send another shape.
    if let Some(version) = request
        .params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        && version != u64::from(CONTROLLER_PROTOCOL_VERSION)
    {
        return RpcResponse::failure(
            id,
            ProtocolError::version_mismatch(
                CONTROLLER_PROTOCOL_VERSION,
                u32::try_from(version).unwrap_or(u32::MAX),
            ),
        );
    }
    match request.params::<ControllerInitializeParams>() {
        Ok(_) => {
            *initialized = true;
            RpcResponse::success(request.id.clone(), app.initialize.as_ref())
        }
        Err(error) => RpcResponse::failure(id, error),
    }
}

async fn handle<B: IncusBackend>(app: &App<B>, request: Request) -> RpcResponse {
    let result = route(app, &request).await;
    RpcResponse {
        id: Some(request.id),
        result,
    }
}

async fn route<B: IncusBackend>(app: &App<B>, request: &Request) -> Result<Value, ProtocolError> {
    let Some(spec) = manifest::find(Plane::Controller, &request.method)
        .filter(|spec| spec.kind == MessageKind::Request)
    else {
        return Err(ProtocolError::method_not_found(&request.method));
    };
    if spec.capability == Some(Capability::Ingress) && !app.initialize.capabilities.ingress {
        return Err(ProtocolError::capability_unavailable(
            Capability::Ingress,
            "this provider has no public ingress configured",
        ));
    }
    match request.method.as_str() {
        TemplateList::NAME => encode(list_templates(app, request.params()?)),
        TargetCreate::NAME => encode(create_target(app, request.params()?).await?),
        TargetGet::NAME => encode(get_target(app, request.params()?).await?),
        TargetSetPower::NAME => encode(set_target_power(app, request.params()?).await?),
        TargetClose::NAME => encode(close_target(app, request.params()?).await?),
        IngressEnsure::NAME => encode(ensure_ingress(app, request.params()?).await?),
        IngressRemove::NAME => encode(remove_ingress(app, request.params()?).await?),
        _ => Err(ProtocolError::method_not_found(&request.method)),
    }
}

fn encode<T: Serialize>(value: T) -> Result<Value, ProtocolError> {
    serde_json::to_value(value)
        .map_err(|error| ProtocolError::internal(format!("encode result: {error}")))
}

/// Every universe gets the same provider-wide templates.
fn list_templates<B>(app: &App<B>, _params: ListTemplatesParams) -> ListTemplatesResponse {
    ListTemplatesResponse {
        templates: app
            .config
            .templates
            .values()
            .map(|template| EnvironmentTemplate {
                template_id: template.template_id.clone(),
                display_name: template.display_name.clone(),
                description: template.description.clone(),
                power_states: SUPPORTED_POWER_STATES.to_vec(),
                public_ingress: template.public_ingress,
                idle: template.idle.clone(),
                deprecated: template.deprecated,
                resources: Some(template.resources.clone()),
                metadata: display_metadata([(
                    "imageFingerprint",
                    template.image_fingerprint.as_str(),
                )]),
            })
            .collect(),
    }
}

async fn get_target<B: IncusBackend>(
    app: &App<B>,
    params: GetTargetParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = &params.target.owner;
    let name = resolve(app, &params.target).await?;
    let observed = observe(app, owner, &name).await?;
    observation(app, owner, &name, &observed)
        .map(|target| TargetResponse { target })
        .ok_or_else(|| ProtocolError::not_found(format!("no target exists for owner {name}")))
}

/// The wire path of an idle stage's threshold in `target/create` params.
fn idle_stage_field(action: IdleAction) -> &'static str {
    match action {
        IdleAction::Pause => "idle.pauseAfterMs",
        IdleAction::Suspend => "idle.suspendAfterMs",
        IdleAction::Stop => "idle.stopAfterMs",
        IdleAction::Close => "idle.closeAfterMs",
    }
}

async fn create_target<B: IncusBackend>(
    app: &App<B>,
    params: CreateTargetParams,
) -> Result<TargetResponse, ProtocolError> {
    if let Some(idle) = &params.idle {
        idle.validate()
            .map_err(|error| ProtocolError::invalid_field("idle", error.to_string()))?;
        if let Some(action) = unsupported_idle_stage(idle) {
            return Err(ProtocolError::invalid_field(
                idle_stage_field(action),
                format!("the Incus provider cannot carry out the {action} stage"),
            ));
        }
    }
    let owner = params.owner;
    let name = policy::instance_name(&owner);
    let observed = observe(app, &owner, &name).await?;
    if observed.holds_target() {
        // Idempotent per owner: an owner that holds a target gets it back,
        // and no creation policy runs again.
        return observed_response(app, &owner, &name, &observed);
    }
    let template = app
        .config
        .templates
        .get(&params.template_id)
        .ok_or_else(|| {
            ProtocolError::not_found(format!(
                "template {} is not offered by this provider",
                params.template_id
            ))
        })?;
    if template.deprecated {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::TemplateDeprecated,
            format!(
                "template {} is deprecated and creates no new targets",
                template.template_id
            ),
        ));
    }
    let template = template.clone();
    // The target keeps the policy it was created with, even when the
    // template's default changes later.
    let idle = params.idle.or_else(|| template.idle.clone());
    let backend = app.backend.clone();
    let job_owner = owner.clone();
    let job_name = name.clone();
    let job = async move {
        let universe_id = &job_owner.universe_id;
        if backend.get_owned(universe_id, &job_name).await?.is_some() {
            return Ok(());
        }
        backend.reconcile_universe(universe_id).await?;
        backend.ensure_image(&template.image_fingerprint).await?;
        backend
            .create_vm(&job_owner, &template, idle.as_ref())
            .await
    };
    let target = mutate(app, &owner, &name, WorkKind::Create, job.boxed())
        .await?
        .ok_or_else(|| ProtocolError::unavailable("the created Incus instance is not visible"))?;
    Ok(TargetResponse { target })
}

async fn set_target_power<B: IncusBackend>(
    app: &App<B>,
    params: SetTargetPowerParams,
) -> Result<TargetResponse, ProtocolError> {
    let power = params.power;
    if !SUPPORTED_POWER_STATES.contains(&power) {
        return Err(ProtocolError::invalid_field(
            "power",
            format!("this provider cannot move a target to {power}"),
        ));
    }
    let owner = &params.target.owner;
    let name = resolve(app, &params.target).await?;
    let observed = observe(app, owner, &name).await?;
    let current = observation(app, owner, &name, &observed)
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} does not exist")))?;
    if current.phase != TargetPhase::Active {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::TargetNotActive,
            format!("target {name} is {:?}, not active", current.phase),
        ));
    }
    if current.power == Some(power) && !observed.slot.as_ref().is_some_and(Slot::running) {
        return observed_response(app, owner, &name, &observed);
    }
    let job = power_job(&app.backend, &owner.universe_id, &name, power);
    let target = mutate(app, owner, &name, WorkKind::Power(power), job)
        .await?
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} disappeared")))?;
    Ok(TargetResponse { target })
}

async fn close_target<B: IncusBackend>(
    app: &App<B>,
    params: CloseTargetParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = &params.target.owner;
    let name = resolve(app, &params.target).await?;
    let observed = observe(app, owner, &name).await?;
    if !observed.holds_target() {
        app.work.forget(&name);
        return Ok(TargetResponse {
            target: closed(owner),
        });
    }
    app.work.start(
        &name,
        WorkKind::Close,
        close_job(&app.backend, &owner.universe_id, &name),
    );
    wait_for_work(app, &name).await;
    let observed = observe(app, owner, &name).await?;
    if !observed.holds_target() {
        return Ok(TargetResponse {
            target: closed(owner),
        });
    }
    let mut target = observation(app, owner, &name, &observed).unwrap_or_else(|| closed(owner));
    // Until the instance is gone, a close answers closing, with the error of
    // a failed attempt as its message; the next close tries again.
    target.phase = TargetPhase::Closing;
    target.power = None;
    Ok(TargetResponse { target })
}

/// The background job that moves the instance `name` toward `power`.
pub(crate) fn power_job<B: IncusBackend>(
    backend: &B,
    universe_id: &UniverseId,
    name: &str,
    power: PowerState,
) -> BoxFuture<'static, anyhow::Result<()>> {
    let backend = backend.clone();
    let universe_id = universe_id.clone();
    let name = name.to_owned();
    async move {
        let target = backend
            .get_owned(&universe_id, &name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the Incus instance disappeared"))?;
        backend.set_power(&target, power).await
    }
    .boxed()
}

/// The background job that destroys the instance `name`, if it still
/// exists.
pub(crate) fn close_job<B: IncusBackend>(
    backend: &B,
    universe_id: &UniverseId,
    name: &str,
) -> BoxFuture<'static, anyhow::Result<()>> {
    let backend = backend.clone();
    let universe_id = universe_id.clone();
    let name = name.to_owned();
    async move {
        match backend.get_owned(&universe_id, &name).await? {
            Some(target) => backend.delete_vm(&target).await,
            None => Ok(()),
        }
    }
    .boxed()
}

async fn ensure_ingress<B: IncusBackend>(
    app: &App<B>,
    params: EnsureIngressParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = &params.target.owner;
    let name = resolve(app, &params.target).await?;
    let observed = observe(app, owner, &name).await?;
    let Some(instance) = ingress_subject(app, owner, &name, &observed)? else {
        return observed_response(app, owner, &name, &observed);
    };
    let port = TemplateId::new(instance.template_id.as_str())
        .ok()
        .and_then(|template_id| app.config.templates.get(&template_id))
        .and_then(|template| template.ingress_port)
        .ok_or_else(|| {
            ProtocolError::failed_precondition(
                PreconditionReason::IngressUnsupported,
                format!("the template of target {name} has no public ingress"),
            )
        })?;
    let hostname = ingress::allocated_hostname(&app.config, &instance).map_err(|error| {
        ProtocolError::internal(format!("allocate ingress hostname: {error:#}"))
    })?;
    if instance.ingress_hostname.as_deref() == Some(hostname.as_str())
        && instance.ingress_port == Some(port)
    {
        return observed_response(app, owner, &name, &observed);
    }
    let backend = app.backend.clone();
    let universe_id = owner.universe_id.clone();
    let job_name = name.clone();
    let job = async move {
        let target = backend
            .get_owned(&universe_id, &job_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the Incus instance disappeared"))?;
        backend.set_ingress(&target, Some((&hostname, port))).await
    };
    let target = mutate(
        app,
        owner,
        &name,
        WorkKind::Ingress { enabled: true },
        job.boxed(),
    )
    .await?
    .ok_or_else(|| ProtocolError::not_found(format!("target {name} disappeared")))?;
    Ok(TargetResponse { target })
}

async fn remove_ingress<B: IncusBackend>(
    app: &App<B>,
    params: RemoveIngressParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = &params.target.owner;
    let name = resolve(app, &params.target).await?;
    let observed = observe(app, owner, &name).await?;
    let Some(instance) = ingress_subject(app, owner, &name, &observed)? else {
        return observed_response(app, owner, &name, &observed);
    };
    if instance.ingress_hostname.is_none() && instance.ingress_port.is_none() {
        return observed_response(app, owner, &name, &observed);
    }
    let backend = app.backend.clone();
    let universe_id = owner.universe_id.clone();
    let job_name = name.clone();
    let job = async move {
        let target = backend
            .get_owned(&universe_id, &job_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the Incus instance disappeared"))?;
        backend.set_ingress(&target, None).await
    };
    let target = mutate(
        app,
        owner,
        &name,
        WorkKind::Ingress { enabled: false },
        job.boxed(),
    )
    .await?
    .ok_or_else(|| ProtocolError::not_found(format!("target {name} disappeared")))?;
    Ok(TargetResponse { target })
}

/// The instance whose ingress may change, `None` when the target is closing
/// (the caller answers with the observation), or an error when there is no
/// instance yet or at all.
fn ingress_subject<B>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    observed: &Observed,
) -> Result<Option<OwnedTarget>, ProtocolError> {
    let current = observation(app, owner, name, observed)
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} does not exist")))?;
    if current.phase == TargetPhase::Closing {
        return Ok(None);
    }
    observed.instance.clone().map(Some).ok_or_else(|| {
        ProtocolError::failed_precondition(
            PreconditionReason::TargetNotActive,
            format!("target {name} has no instance yet"),
        )
    })
}

/// The instance name a reference resolves to. The provider assigns exactly
/// one name per owner; a `targetId` naming any other instance is another
/// owner's target or none at all.
async fn resolve<B: IncusBackend>(
    app: &App<B>,
    target: &TargetRef,
) -> Result<String, ProtocolError> {
    let name = policy::instance_name(&target.owner);
    if let Some(target_id) = &target.target_id
        && target_id.as_str() != name
    {
        // A target being created is known by its job before its instance
        // exists.
        let foreign = app.work.view(target_id.as_str()).is_some()
            || app
                .backend
                .get_owned(&target.owner.universe_id, target_id.as_str())
                .await
                .map_err(unavailable)?
                .is_some();
        return Err(if foreign {
            owner_mismatch(target_id.as_str())
        } else {
            ProtocolError::not_found(format!("target {target_id} does not exist"))
        });
    }
    Ok(name)
}

/// What the provider knows about one owner's target.
struct Observed {
    instance: Option<OwnedTarget>,
    slot: Option<Slot>,
}

impl Observed {
    /// The owner holds something: an instance or work in progress.
    fn holds_target(&self) -> bool {
        self.instance.is_some() || self.slot.as_ref().is_some_and(Slot::running)
    }
}

async fn observe<B: IncusBackend>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
) -> Result<Observed, ProtocolError> {
    let instance = app
        .backend
        .get_owned(&owner.universe_id, name)
        .await
        .map_err(unavailable)?;
    if let Some(instance) = &instance
        && instance.owner != *owner
    {
        return Err(owner_mismatch(name));
    }
    Ok(Observed {
        instance,
        slot: app.work.view(name),
    })
}

fn observed_response<B>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    observed: &Observed,
) -> Result<TargetResponse, ProtocolError> {
    observation(app, owner, name, observed)
        .map(|target| TargetResponse { target })
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} does not exist")))
}

/// The phase, power and message of an observation.
struct Condition {
    phase: TargetPhase,
    power: Option<PowerState>,
    message: Option<DescriptiveText>,
}

impl Condition {
    fn phase(phase: TargetPhase, message: &str) -> Self {
        Self {
            phase,
            power: None,
            message: DescriptiveText::lossy(message),
        }
    }
}

/// The observation of a target, or `None` when the owner holds nothing.
///
/// `targetId` is reported only once the Incus instance exists. Until then
/// the runtime keeps sending the idempotent `target/create`, which starts
/// the job again when it failed or was lost with a provider restart; a
/// failed attempt is reported as `provisioning` with its error as
/// `message`, so the runtime retries it until its provisioning deadline
/// instead of failing at the first Incus hiccup. Whether envd answers is
/// not part of an observation: the runtime's open handshake over the data
/// route proves it.
fn observation<B>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    observed: &Observed,
) -> Option<TargetObservation> {
    let failure = observed.slot.as_ref().and_then(Slot::failure);
    let running = observed
        .slot
        .as_ref()
        .filter(|slot| slot.running())
        .map(|slot| slot.kind);
    let failed_creation = observed
        .slot
        .as_ref()
        .is_some_and(|slot| !slot.running() && slot.kind == WorkKind::Create);
    let condition = match (running, &observed.instance) {
        (Some(WorkKind::Close), _) => {
            Condition::phase(TargetPhase::Closing, "closing the Incus instance")
        }
        (Some(WorkKind::Create), _) => Condition::phase(
            TargetPhase::Provisioning,
            "creating and starting the Incus instance",
        ),
        (_, Some(instance)) => {
            let mut condition = instance_condition(instance);
            if condition.message.is_none() {
                condition.message = failure.and_then(DescriptiveText::lossy);
            }
            condition
        }
        (_, None) if failed_creation => Condition::phase(
            TargetPhase::Provisioning,
            &format!(
                "the last attempt failed and the next target/create retries it: {}",
                failure.unwrap_or("unknown error")
            ),
        ),
        (_, None) => return None,
    };
    let target_id = match &observed.instance {
        Some(_) => Some(ProviderTargetId::new(name).ok()?),
        None => None,
    };
    let ingress = observed
        .instance
        .as_ref()
        .and_then(|instance| instance.ingress_hostname.as_deref())
        .and_then(|hostname| ingress::public_endpoint(&app.config, hostname).ok())
        .map(|endpoint| IngressObservation { endpoint });
    Some(TargetObservation {
        target_id,
        owner: owner.clone(),
        phase: condition.phase,
        power: condition.power,
        message: condition.message,
        ingress,
    })
}

const INSTANCE_ERROR: &str = "Incus reports the instance in the Error state";

/// The phase and power an Incus instance status maps to. A starting VM is
/// reported running and a stopping one stopped: `power` names where the
/// target is going.
fn instance_condition(instance: &OwnedTarget) -> Condition {
    let active = |power| Condition {
        phase: TargetPhase::Active,
        power: Some(power),
        message: None,
    };
    match instance.status {
        InstanceStatus::Running | InstanceStatus::Starting => active(PowerState::Running),
        InstanceStatus::Frozen => active(PowerState::Paused),
        InstanceStatus::Stopped | InstanceStatus::Stopping => active(PowerState::Stopped),
        InstanceStatus::Error => Condition::phase(TargetPhase::Failed, INSTANCE_ERROR),
        InstanceStatus::Other => Condition::phase(
            TargetPhase::Unknown,
            "Incus reports an instance status this provider does not recognize",
        ),
    }
}

fn closed(owner: &TargetOwner) -> TargetObservation {
    TargetObservation {
        target_id: None,
        owner: owner.clone(),
        phase: TargetPhase::Closed,
        power: None,
        message: None,
        ingress: None,
    }
}

/// Provider display facts; entries outside the metadata bounds are left out.
fn display_metadata<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Metadata {
    let entries: BTreeMap<String, String> = entries
        .into_iter()
        .filter(|(key, value)| validate_metadata_entry(key, value).is_ok())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    Metadata::new(entries).unwrap_or_default()
}

fn unavailable(error: anyhow::Error) -> ProtocolError {
    ProtocolError::unavailable(format!("Incus: {error:#}"))
}

fn owner_mismatch(target_id: &str) -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::OwnerMismatch,
        format!("target {target_id} belongs to another owner"),
    )
}

/// Start `job` for the target and wait up to the prompt bound, then
/// observe.
async fn mutate<B: IncusBackend>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    kind: WorkKind,
    job: BoxFuture<'static, anyhow::Result<()>>,
) -> Result<Option<TargetObservation>, ProtocolError> {
    app.work.start(name, kind, job);
    wait_for_work(app, name).await;
    let observed = observe(app, owner, name).await?;
    Ok(observation(app, owner, name, &observed))
}

async fn wait_for_work<B>(app: &App<B>, name: &str) {
    if let Some(mut done) = app.work.done(name) {
        let _ = tokio::time::timeout(PROMPT_WAIT, done.wait_for(|finished| *finished)).await;
    }
}

/// The background job kinds; at most one job per target is current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkKind {
    Create,
    Power(PowerState),
    Ingress { enabled: bool },
    Close,
}

#[derive(Clone, Debug)]
enum SlotState {
    Running,
    Failed(String),
}

/// The current job of one target: running, or failed with its error. A job
/// that succeeds leaves no slot, because Incus then holds the truth.
#[derive(Clone, Debug)]
struct Slot {
    id: u64,
    kind: WorkKind,
    state: SlotState,
    done: watch::Receiver<bool>,
}

impl Slot {
    fn running(&self) -> bool {
        matches!(self.state, SlotState::Running)
    }

    fn failure(&self) -> Option<&str> {
        match &self.state {
            SlotState::Failed(message) => Some(message),
            SlotState::Running => None,
        }
    }
}

/// Background jobs by instance name. Jobs of one target run one at a time
/// under that target's lock; a newer request of another kind replaces the
/// current slot (a close supersedes a running create) and its job runs once
/// the earlier one finished.
#[derive(Default)]
pub(crate) struct Work {
    next_id: AtomicU64,
    inner: StdMutex<WorkInner>,
}

#[derive(Default)]
struct WorkInner {
    slots: HashMap<String, Slot>,
    locks: HashMap<String, Arc<AsyncMutex<()>>>,
}

impl Work {
    fn lock(&self) -> std::sync::MutexGuard<'_, WorkInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run `job` as the target's current job unless a job of the same kind
    /// is already running.
    pub(crate) fn start(
        self: &Arc<Self>,
        name: &str,
        kind: WorkKind,
        job: BoxFuture<'static, anyhow::Result<()>>,
    ) {
        let mut inner = self.lock();
        if inner
            .slots
            .get(name)
            .is_some_and(|slot| slot.kind == kind && slot.running())
        {
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (finished, done) = watch::channel(false);
        let lock = inner.locks.entry(name.to_owned()).or_default().clone();
        inner.slots.insert(
            name.to_owned(),
            Slot {
                id,
                kind,
                state: SlotState::Running,
                done,
            },
        );
        drop(inner);
        let work = Arc::clone(self);
        let name = name.to_owned();
        tokio::spawn(async move {
            let outcome = {
                let _guard = lock.lock().await;
                AssertUnwindSafe(job)
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("the background job panicked")))
            };
            drop(lock);
            work.finish(&name, id, outcome);
            let _ = finished.send(true);
        });
    }

    fn finish(&self, name: &str, id: u64, outcome: anyhow::Result<()>) {
        let mut inner = self.lock();
        if inner.slots.get(name).is_some_and(|slot| slot.id == id) {
            match outcome {
                Ok(()) => {
                    inner.slots.remove(name);
                }
                Err(error) => {
                    if let Some(slot) = inner.slots.get_mut(name) {
                        slot.state = SlotState::Failed(format!("{error:#}"));
                    }
                }
            }
        }
        Self::release_lock(&mut inner, name);
    }

    /// Drop the target's remembered failure once it holds nothing.
    fn forget(&self, name: &str) {
        let mut inner = self.lock();
        if inner.slots.get(name).is_some_and(|slot| !slot.running()) {
            inner.slots.remove(name);
        }
        Self::release_lock(&mut inner, name);
    }

    fn release_lock(inner: &mut WorkInner, name: &str) {
        if !inner.slots.contains_key(name)
            && inner
                .locks
                .get(name)
                .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            inner.locks.remove(name);
        }
    }

    fn view(&self, name: &str) -> Option<Slot> {
        self.lock().slots.get(name).cloned()
    }

    /// True while a job of the target runs.
    pub(crate) fn busy(&self, name: &str) -> bool {
        self.lock().slots.get(name).is_some_and(Slot::running)
    }

    fn done(&self, name: &str) -> Option<watch::Receiver<bool>> {
        self.lock()
            .slots
            .get(name)
            .filter(|slot| slot.running())
            .map(|slot| slot.done.clone())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use environment_client::{ConnectOptions, DataClient, conformance::data_route::check_refusal};
    use environment_protocol::{
        error::{ErrorData, ProtocolErrorKind},
        idle::IdlePolicy,
        jsonrpc::Id,
    };

    use super::*;
    use crate::{
        IncusTopology,
        config::{
            ConfigInner, IncusConfig, IncusMode, NetworkPolicy, ProviderCredential, TemplatePolicy,
        },
        incus::IncusMemberStatus,
        policy::tests::owner,
    };

    pub(crate) const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef";

    /// An in-memory Incus: instances by name, every mutation instant.
    #[derive(Clone, Default)]
    pub(crate) struct FakeBackend {
        pub(crate) instances: Arc<StdMutex<BTreeMap<String, OwnedTarget>>>,
        pub(crate) creates: Arc<AtomicUsize>,
        pub(crate) deletes: Arc<AtomicUsize>,
        /// Every `set_power` target state, in order.
        pub(crate) power_calls: Arc<StdMutex<Vec<PowerState>>>,
    }

    impl FakeBackend {
        pub(crate) fn insert(
            &self,
            owner: &TargetOwner,
            template_id: &str,
            status: InstanceStatus,
        ) {
            let name = policy::instance_name(owner);
            self.instances.lock().unwrap().insert(
                name.clone(),
                OwnedTarget {
                    target_id: ProviderTargetId::new(name).unwrap(),
                    owner: owner.clone(),
                    template_id: template_id.to_owned(),
                    idle: None,
                    status,
                    ipv4_address: None,
                    ingress_hostname: None,
                    ingress_port: None,
                    location: None,
                },
            );
        }

        pub(crate) fn update(&self, owner: &TargetOwner, change: impl FnOnce(&mut OwnedTarget)) {
            let name = policy::instance_name(owner);
            change(self.instances.lock().unwrap().get_mut(&name).unwrap());
        }

        pub(crate) fn get(&self, owner: &TargetOwner) -> Option<OwnedTarget> {
            let name = policy::instance_name(owner);
            self.instances.lock().unwrap().get(&name).cloned()
        }
    }

    #[async_trait]
    impl IncusBackend for FakeBackend {
        async fn topology(&self) -> anyhow::Result<IncusTopology> {
            Ok(IncusTopology {
                mode: "single",
                members: vec![IncusMemberStatus {
                    name: "standalone".to_owned(),
                    status: "Online".to_owned(),
                    schedulable: true,
                }],
            })
        }

        async fn reconcile_universe(&self, _universe_id: &UniverseId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn ensure_image(&self, _fingerprint: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn list_all_owned(&self) -> anyhow::Result<Vec<OwnedTarget>> {
            Ok(self.instances.lock().unwrap().values().cloned().collect())
        }

        async fn get_owned(
            &self,
            universe_id: &UniverseId,
            name: &str,
        ) -> anyhow::Result<Option<OwnedTarget>> {
            Ok(self
                .instances
                .lock()
                .unwrap()
                .get(name)
                .filter(|target| target.universe() == universe_id)
                .cloned())
        }

        async fn create_vm(
            &self,
            owner: &TargetOwner,
            template: &TemplatePolicy,
            idle: Option<&IdlePolicy>,
        ) -> anyhow::Result<()> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.insert(
                owner,
                template.template_id.as_str(),
                InstanceStatus::Running,
            );
            self.update(owner, |target| target.idle = idle.cloned());
            Ok(())
        }

        async fn delete_vm(&self, target: &OwnedTarget) -> anyhow::Result<()> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            self.instances.lock().unwrap().remove(target.name());
            Ok(())
        }

        async fn set_ingress(
            &self,
            target: &OwnedTarget,
            route: Option<(&str, u16)>,
        ) -> anyhow::Result<()> {
            if let Some(instance) = self.instances.lock().unwrap().get_mut(target.name()) {
                instance.ingress_hostname = route.map(|(hostname, _)| hostname.to_owned());
                instance.ingress_port = route.map(|(_, port)| port);
            }
            Ok(())
        }

        async fn set_power(&self, target: &OwnedTarget, power: PowerState) -> anyhow::Result<()> {
            self.power_calls.lock().unwrap().push(power);
            if let Some(instance) = self.instances.lock().unwrap().get_mut(target.name()) {
                instance.status = match power {
                    PowerState::Running => InstanceStatus::Running,
                    PowerState::Paused => InstanceStatus::Frozen,
                    _ => InstanceStatus::Stopped,
                };
            }
            Ok(())
        }
    }

    pub(crate) fn template(id: &str, deprecated: bool) -> TemplatePolicy {
        TemplatePolicy {
            template_id: TemplateId::new(id).unwrap(),
            display_name: environment_protocol::metadata::DisplayName::new("Small").unwrap(),
            description: None,
            image_fingerprint: "fingerprint".to_owned(),
            cpu: 2,
            memory: "4GiB".to_owned(),
            disk: "40GiB".to_owned(),
            cluster_group: None,
            public_ingress: false,
            ingress_port: None,
            deprecated,
            idle: None,
            resources: Default::default(),
        }
    }

    /// A provider config whose guests' envd listens on `envd_port`.
    pub(crate) fn config_with(envd_port: u16, credential: Option<&str>) -> Config {
        let mut idling = template("idling", false);
        idling.idle = Some(IdlePolicy {
            pause_after_ms: Some(600_000),
            ..IdlePolicy::default()
        });
        let templates = [template("small", false), template("retired", true), idling]
            .into_iter()
            .map(|template| (template.template_id.clone(), template))
            .collect();
        Config(Arc::new(ConfigInner {
            controller_listen: "127.0.0.1:0".parse().unwrap(),
            credential: credential.map(|credential| ProviderCredential::new(credential).unwrap()),
            incus: IncusConfig {
                mode: IncusMode::Single,
                endpoints: vec!["https://incus.test".to_owned()],
                client_certificate_pem: "cert".into(),
                client_private_key_pem: "key".into(),
                server_ca_pem: "ca".into(),
                storage_pool: "default".to_owned(),
                cluster_network_uplink: None,
                image_server_url: None,
            },
            templates,
            network: NetworkPolicy::default(),
            relay_idle_seconds: 60,
            dial_timeout_seconds: 1,
            idle_check_seconds: 60,
            envd_port,
            ingress: None,
        }))
    }

    fn config() -> Config {
        config_with(19091, Some(CREDENTIAL))
    }

    fn app() -> (App<FakeBackend>, FakeBackend) {
        let backend = FakeBackend::default();
        (App::new(config(), backend.clone()), backend)
    }

    async fn call<P: Serialize>(
        app: &App<FakeBackend>,
        method: &str,
        params: &P,
    ) -> Result<TargetObservation, ProtocolError> {
        let request = Request::new(Id::Number(1), method, params).unwrap();
        let value = route(app, &request).await?;
        Ok(serde_json::from_value::<TargetResponse>(value)
            .unwrap()
            .target)
    }

    fn by_owner(owner: &TargetOwner) -> TargetRef {
        TargetRef {
            owner: owner.clone(),
            target_id: None,
        }
    }

    fn create(owner: &TargetOwner, template: &str) -> CreateTargetParams {
        CreateTargetParams {
            owner: owner.clone(),
            template_id: TemplateId::new(template).unwrap(),
            display_name: None,
            idle: None,
        }
    }

    fn reason(error: &ProtocolError) -> Option<PreconditionReason> {
        match error.data {
            ErrorData::FailedPrecondition { reason } => Some(reason),
            _ => None,
        }
    }

    #[test]
    fn initialize_enforces_version_and_runs_once() {
        let (app, _) = app();
        let mut initialized = false;
        let params = |version: u32| serde_json::json!({"protocolVersion": version, "clientInfo": {"name": "test"}});
        let other = CONTROLLER_PROTOCOL_VERSION + 1;
        let request = Request::new(Id::Number(1), "initialize", &params(other)).unwrap();
        let mismatch = initialize(&app, &mut initialized, &request)
            .result
            .unwrap_err();
        assert_eq!(
            mismatch.data,
            ErrorData::VersionMismatch {
                expected: CONTROLLER_PROTOCOL_VERSION,
                received: other
            }
        );
        assert!(!initialized);

        let request = Request::new(
            Id::Number(2),
            "initialize",
            &params(CONTROLLER_PROTOCOL_VERSION),
        )
        .unwrap();
        let response: ControllerInitializeResponse = initialize(&app, &mut initialized, &request)
            .decode()
            .unwrap();
        assert!(initialized);
        assert_eq!(response.plane, Plane::Controller);
        assert!(response.capabilities.power);
        assert!(!response.capabilities.ingress);

        let repeat = initialize(&app, &mut initialized, &request)
            .result
            .unwrap_err();
        assert_eq!(repeat.kind(), ProtocolErrorKind::InvalidRequest);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn templates_offer_power_states_and_their_idle_default() {
        let (app, _) = app();
        let request = Request::new(
            Id::Number(1),
            TemplateList::NAME,
            &ListTemplatesParams {
                universe_id: UniverseId::new("u").unwrap(),
            },
        )
        .unwrap();
        let listed: ListTemplatesResponse =
            serde_json::from_value(route(&app, &request).await.unwrap()).unwrap();
        let idling = listed
            .templates
            .iter()
            .find(|template| template.template_id.as_str() == "idling")
            .unwrap();
        assert_eq!(idling.power_states, SUPPORTED_POWER_STATES);
        assert_eq!(
            idling.idle.as_ref().and_then(|idle| idle.pause_after_ms),
            Some(600_000)
        );
        let small = listed
            .templates
            .iter()
            .find(|template| template.template_id.as_str() == "small")
            .unwrap();
        assert_eq!(small.idle, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_and_unadvertised_methods_are_typed_errors() {
        let (app, _) = app();
        let unknown = call(&app, "controller/listTargets", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(unknown.kind(), ProtocolErrorKind::MethodNotFound);
        let adopt = call(&app, "target/adopt", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(adopt.kind(), ProtocolErrorKind::MethodNotFound);
        let ingress = call(
            &app,
            IngressEnsure::NAME,
            &EnsureIngressParams {
                target: by_owner(&owner("u", "e")),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            ingress.data,
            ErrorData::CapabilityUnavailable {
                capability: Capability::Ingress
            }
        );
        let undecodable = call(&app, TargetGet::NAME, &serde_json::json!({"target": 7}))
            .await
            .unwrap_err();
        assert_eq!(undecodable.kind(), ProtocolErrorKind::InvalidParams);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_never_creates_and_answers_not_found() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        for _ in 0..2 {
            let error = call(
                &app,
                TargetGet::NAME,
                &GetTargetParams {
                    target: by_owner(&owner),
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), ProtocolErrorKind::NotFound);
        }
        let unknown = call(
            &app,
            TargetGet::NAME,
            &GetTargetParams {
                target: TargetRef {
                    owner: owner.clone(),
                    target_id: Some(ProviderTargetId::new("default/pod-abc").unwrap()),
                },
            },
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.kind(), ProtocolErrorKind::NotFound);
        assert_eq!(backend.creates.load(Ordering::SeqCst), 0);
        assert!(backend.instances.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_is_idempotent_per_owner_and_applies_policy_only_once() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        let params = create(&owner, "small");
        let first = call(&app, TargetCreate::NAME, &params).await.unwrap();
        let second = call(&app, TargetCreate::NAME, &params).await.unwrap();
        assert_eq!(first.owner, owner);
        assert_eq!(first.target_id, second.target_id);
        assert_eq!(
            first.target_id.as_ref().map(ProviderTargetId::as_str),
            Some(policy::instance_name(&owner).as_str())
        );
        assert_eq!(second.phase, TargetPhase::Active);
        assert_eq!(second.power, Some(PowerState::Running));
        assert_eq!(backend.creates.load(Ordering::SeqCst), 1);

        // A target that already exists comes back even when its template
        // has been deprecated since: observation applies no creation policy.
        let existing = crate::policy::tests::owner("u", "old");
        backend.insert(&existing, "retired", InstanceStatus::Stopped);
        let observed = call(&app, TargetCreate::NAME, &create(&existing, "retired"))
            .await
            .unwrap();
        assert_eq!(observed.power, Some(PowerState::Stopped));
        assert_eq!(backend.creates.load(Ordering::SeqCst), 1);

        let fresh = crate::policy::tests::owner("u", "new");
        let refused = call(&app, TargetCreate::NAME, &create(&fresh, "retired"))
            .await
            .unwrap_err();
        assert_eq!(
            reason(&refused),
            Some(PreconditionReason::TemplateDeprecated)
        );
        let missing = call(&app, TargetCreate::NAME, &create(&fresh, "absent"))
            .await
            .unwrap_err();
        assert_eq!(missing.kind(), ProtocolErrorKind::NotFound);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_records_the_override_or_the_template_idle_default() {
        let (app, backend) = app();
        let defaulted = owner("u", "defaulted");
        call(&app, TargetCreate::NAME, &create(&defaulted, "idling"))
            .await
            .unwrap();
        assert_eq!(
            backend.get(&defaulted).unwrap().idle,
            app.config.templates[&TemplateId::new("idling").unwrap()]
                .idle
                .clone()
        );

        let overridden = owner("u", "overridden");
        let idle = IdlePolicy {
            stop_after_ms: Some(1_800_000),
            ..IdlePolicy::default()
        };
        let mut params = create(&overridden, "idling");
        params.idle = Some(idle.clone());
        call(&app, TargetCreate::NAME, &params).await.unwrap();
        assert_eq!(backend.get(&overridden).unwrap().idle, Some(idle));

        let none = owner("u", "none");
        call(&app, TargetCreate::NAME, &create(&none, "small"))
            .await
            .unwrap();
        assert_eq!(backend.get(&none).unwrap().idle, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_refuses_an_invalid_idle_override() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        let mut params = create(&owner, "small");
        params.idle = Some(IdlePolicy {
            pause_after_ms: Some(600_000),
            stop_after_ms: Some(60_000),
            ..IdlePolicy::default()
        });
        let refused = call(&app, TargetCreate::NAME, &params).await.unwrap_err();
        assert_eq!(refused.kind(), ProtocolErrorKind::InvalidParams);
        assert_eq!(backend.creates.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_refuses_an_idle_stage_incus_cannot_reach() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        let mut params = create(&owner, "small");
        params.idle = Some(IdlePolicy {
            pause_after_ms: Some(60_000),
            suspend_after_ms: Some(600_000),
            ..IdlePolicy::default()
        });
        let refused = call(&app, TargetCreate::NAME, &params).await.unwrap_err();
        assert_eq!(refused.kind(), ProtocolErrorKind::InvalidParams);
        assert!(matches!(
            &refused.data,
            ErrorData::InvalidParams { field: Some(field), .. } if field == "idle.suspendAfterMs"
        ));
        assert_eq!(backend.creates.load(Ordering::SeqCst), 0);
        assert!(backend.get(&owner).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_foreign_target_id_is_an_owner_mismatch() {
        let (app, backend) = app();
        let holder = owner("u", "holder");
        backend.insert(&holder, "small", InstanceStatus::Running);
        let error = call(
            &app,
            TargetGet::NAME,
            &GetTargetParams {
                target: TargetRef {
                    owner: owner("u", "other"),
                    target_id: Some(ProviderTargetId::new(policy::instance_name(&holder)).unwrap()),
                },
            },
        )
        .await
        .unwrap_err();
        assert_eq!(reason(&error), Some(PreconditionReason::OwnerMismatch));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_by_owner_destroys_and_is_idempotent() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        backend.insert(&owner, "small", InstanceStatus::Running);
        let closed = call(
            &app,
            TargetClose::NAME,
            &CloseTargetParams {
                target: by_owner(&owner),
            },
        )
        .await
        .unwrap();
        assert_eq!(closed.phase, TargetPhase::Closed);
        assert_eq!(closed.target_id, None);
        assert_eq!(closed.owner, owner);
        let again = call(
            &app,
            TargetClose::NAME,
            &CloseTargetParams {
                target: by_owner(&owner),
            },
        )
        .await
        .unwrap();
        assert_eq!(again.phase, TargetPhase::Closed);
        assert_eq!(backend.deletes.load(Ordering::SeqCst), 1);
        let gone = call(
            &app,
            TargetGet::NAME,
            &GetTargetParams {
                target: by_owner(&owner),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(gone.kind(), ProtocolErrorKind::NotFound);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn power_changes_need_an_active_target_and_a_supported_state() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        let set = |power| SetTargetPowerParams {
            target: by_owner(&owner),
            power,
        };
        let missing = call(&app, TargetSetPower::NAME, &set(PowerState::Paused))
            .await
            .unwrap_err();
        assert_eq!(missing.kind(), ProtocolErrorKind::NotFound);

        backend.insert(&owner, "small", InstanceStatus::Running);
        let unsupported = call(&app, TargetSetPower::NAME, &set(PowerState::Suspended))
            .await
            .unwrap_err();
        assert_eq!(unsupported.kind(), ProtocolErrorKind::InvalidParams);
        let paused = call(&app, TargetSetPower::NAME, &set(PowerState::Paused))
            .await
            .unwrap();
        assert_eq!(paused.phase, TargetPhase::Active);
        assert_eq!(paused.power, Some(PowerState::Paused));

        backend.insert(&owner, "small", InstanceStatus::Error);
        let failed = call(&app, TargetSetPower::NAME, &set(PowerState::Running))
            .await
            .unwrap_err();
        assert_eq!(reason(&failed), Some(PreconditionReason::TargetNotActive));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observations_carry_power_only_while_active() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        let get = || GetTargetParams {
            target: by_owner(&owner),
        };
        for (status, power) in [
            (InstanceStatus::Running, Some(PowerState::Running)),
            (InstanceStatus::Starting, Some(PowerState::Running)),
            (InstanceStatus::Frozen, Some(PowerState::Paused)),
            (InstanceStatus::Stopping, Some(PowerState::Stopped)),
            (InstanceStatus::Stopped, Some(PowerState::Stopped)),
        ] {
            backend.insert(&owner, "small", status);
            let observed = call(&app, TargetGet::NAME, &get()).await.unwrap();
            assert_eq!(observed.phase, TargetPhase::Active, "{status:?}");
            assert_eq!(observed.power, power, "{status:?}");
        }
        backend.insert(&owner, "small", InstanceStatus::Error);
        let failed = call(&app, TargetGet::NAME, &get()).await.unwrap();
        assert_eq!(failed.phase, TargetPhase::Failed);
        assert_eq!(failed.power, None);
        assert!(failed.message.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_create_is_provisioning_without_an_id_and_retried_by_create() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        let name = policy::instance_name(&owner);
        app.work.start(
            &name,
            WorkKind::Create,
            async { Err(anyhow::anyhow!("image server unreachable")) }.boxed(),
        );
        wait_for_work(&app, &name).await;
        let observed = call(
            &app,
            TargetGet::NAME,
            &GetTargetParams {
                target: by_owner(&owner),
            },
        )
        .await
        .unwrap();
        // No instance exists, so no id is handed out and the runtime keeps
        // sending target/create.
        assert_eq!(observed.phase, TargetPhase::Provisioning);
        assert_eq!(observed.target_id, None);
        assert!(
            observed
                .message
                .as_ref()
                .is_some_and(|message| message.as_str().contains("image server unreachable"))
        );
        let created = call(&app, TargetCreate::NAME, &create(&owner, "small"))
            .await
            .unwrap();
        assert_eq!(created.phase, TargetPhase::Active);
        assert_eq!(
            created.target_id.as_ref().map(ProviderTargetId::as_str),
            Some(name.as_str())
        );
        assert_eq!(backend.creates.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_an_owner_whose_create_failed_forgets_the_failure() {
        let (app, _) = app();
        let owner = owner("u", "e");
        let name = policy::instance_name(&owner);
        app.work.start(
            &name,
            WorkKind::Create,
            async { Err(anyhow::anyhow!("image pull failed")) }.boxed(),
        );
        wait_for_work(&app, &name).await;
        // Closing an owner whose create failed before any instance existed
        // holds nothing, so it closes at once and forgets the failure.
        let closed = call(
            &app,
            TargetClose::NAME,
            &CloseTargetParams {
                target: by_owner(&owner),
            },
        )
        .await
        .unwrap();
        assert_eq!(closed.phase, TargetPhase::Closed);
        assert!(app.work.view(&name).is_none());
    }

    #[test]
    fn only_the_exact_bearer_credential_is_authorized() {
        let config = config();
        let mut headers = HeaderMap::new();
        assert!(!authorized(&config, &headers));
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!authorized(&config, &headers));
        headers.insert(header::AUTHORIZATION, CREDENTIAL.parse().unwrap());
        assert!(!authorized(&config, &headers));
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {CREDENTIAL}").parse().unwrap(),
        );
        assert!(authorized(&config, &headers));
    }

    #[test]
    fn without_a_credential_every_request_is_authorized() {
        let config = config_with(19091, None);
        let mut headers = HeaderMap::new();
        assert!(authorized(&config, &headers));
        headers.insert(header::AUTHORIZATION, "Bearer anything".parse().unwrap());
        assert!(authorized(&config, &headers));
    }

    /// A guest envd stand-in on loopback that accepts WebSocket upgrades
    /// and answers nothing. Returns its port.
    pub(crate) async fn silent_envd() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await {
                        while let Some(Ok(_)) = socket.next().await {}
                    }
                });
            }
        });
        port
    }

    /// Serve `app`'s routes on loopback with a short data route hold.
    /// Returns the data route URL of `owner`'s target.
    async fn serve_routes(mut app: App<FakeBackend>, owner: &TargetOwner) -> String {
        app.data_route_hold = Duration::from_millis(1_000);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(app)).await });
        format!(
            "ws://{address}/routes/{}/{}/{}",
            owner.universe_id,
            owner.environment_id,
            policy::instance_name(owner)
        )
    }

    fn data_options() -> ConnectOptions {
        ConnectOptions::new(ImplementationInfo::new("test").unwrap()).with_bearer_token(CREDENTIAL)
    }

    /// Dial a data route that must refuse, and return its conforming refusal.
    async fn refused(url: &str) -> DataRouteRefusal {
        let error = match DataClient::connect_initialized(url, data_options()).await {
            Ok(_) => panic!("the data route upgraded"),
            Err(error) => error,
        };
        check_refusal(&error)
            .unwrap_or_else(|problem| panic!("{problem}"))
            .clone()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_data_route_without_a_target_is_gone() {
        let (app, _) = app();
        let owner = owner("u", "e");
        let url = serve_routes(app, &owner).await;
        assert_eq!(refused(&url).await, DataRouteRefusal::Gone {});
        let foreign = format!("{}x", url);
        assert_eq!(refused(&foreign).await, DataRouteRefusal::Gone {});
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_broken_instance_refuses_as_failed() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        backend.insert(&owner, "small", InstanceStatus::Error);
        let url = serve_routes(app, &owner).await;
        assert!(matches!(
            refused(&url).await,
            DataRouteRefusal::Failed { .. }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_running_target_whose_envd_does_not_answer_is_booting() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        // No guest address yet: envd cannot be reached.
        backend.insert(&owner, "small", InstanceStatus::Running);
        let url = serve_routes(app, &owner).await;
        assert_eq!(refused(&url).await, DataRouteRefusal::Booting {});
        assert!(backend.power_calls.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stopped_target_is_woken_and_waking_until_envd_answers() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        backend.insert(&owner, "small", InstanceStatus::Stopped);
        let wakes = app.wakes.clone();
        let url = serve_routes(app, &owner).await;
        assert_eq!(refused(&url).await, DataRouteRefusal::Waking {});
        assert_eq!(
            backend.power_calls.lock().unwrap().as_slice(),
            [PowerState::Running]
        );
        assert_eq!(backend.get(&owner).unwrap().status, InstanceStatus::Running);
        assert!(wakes.within_grace(&policy::instance_name(&owner), Instant::now()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_paused_target_wakes_on_dial_and_relays() {
        let port = silent_envd().await;
        let backend = FakeBackend::default();
        let app = App::new(config_with(port, Some(CREDENTIAL)), backend.clone());
        let relays = app.relays.clone();
        let owner = owner("u", "e");
        backend.insert(&owner, "small", InstanceStatus::Frozen);
        backend.update(&owner, |target| {
            target.ipv4_address = Some("127.0.0.1".to_owned());
        });
        let wakes = app.wakes.clone();
        let url = serve_routes(app, &owner).await;
        let socket = dial_route(&url).await;
        assert_eq!(backend.get(&owner).unwrap().status, InstanceStatus::Running);
        assert_eq!(
            backend.power_calls.lock().unwrap().as_slice(),
            [PowerState::Running]
        );
        // The relay holds the instance while it runs.
        let name = policy::instance_name(&owner);
        assert!(relays.active(&name));
        drop(socket);
        tokio::time::timeout(Duration::from_secs(2), async {
            while relays.active(&name) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the relay releases the instance when it ends");
        // The woken instance has its grace from the end of the connection.
        assert!(wakes.within_grace(&name, Instant::now()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_dial_during_a_power_down_waits_for_the_wake_behind_it() {
        let port = silent_envd().await;
        let backend = FakeBackend::default();
        let app = App::new(config_with(port, Some(CREDENTIAL)), backend.clone());
        let owner = owner("u", "e");
        let name = policy::instance_name(&owner);
        backend.insert(&owner, "small", InstanceStatus::Running);
        backend.update(&owner, |target| {
            target.ipv4_address = Some("127.0.0.1".to_owned());
        });
        // A pause still in progress: the instance reads running until it
        // freezes.
        let pausing = backend.clone();
        let target = backend.get(&owner).unwrap();
        app.work.start(
            &name,
            WorkKind::Power(PowerState::Paused),
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                pausing.set_power(&target, PowerState::Paused).await
            }
            .boxed(),
        );
        let url = serve_routes(app, &owner).await;
        let _socket = dial_route(&url).await;
        // The dial upgraded only after the wake that followed the pause.
        assert_eq!(
            backend.power_calls.lock().unwrap().as_slice(),
            [PowerState::Paused, PowerState::Running]
        );
        assert_eq!(backend.get(&owner).unwrap().status, InstanceStatus::Running);
    }

    /// Open the data route `url` with the configured credential and expect
    /// it to upgrade.
    async fn dial_route(url: &str) -> GuestSocket {
        let mut request =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
                .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {CREDENTIAL}").parse().unwrap(),
        );
        let (socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("the target relays");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        socket
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_data_route_needs_the_configured_credential() {
        let (app, backend) = app();
        let owner = owner("u", "e");
        backend.insert(&owner, "small", InstanceStatus::Running);
        let url = serve_routes(app, &owner).await;
        let options = ConnectOptions::new(ImplementationInfo::new("test").unwrap());
        match DataClient::connect_initialized(&url, options).await {
            Err(environment_client::ClientError::UpgradeRejected { status: 401, .. }) => {}
            Err(error) => panic!("expected HTTP 401, got {error}"),
            Ok(_) => panic!("the data route upgraded without the credential"),
        }
    }

    /// The client's controller conformance suite against this server over
    /// a real WebSocket, with the in-memory backend standing in for Incus.
    #[tokio::test(flavor = "current_thread")]
    async fn the_controller_passes_the_conformance_suite() {
        use environment_client::conformance::controller::{ControllerConformance, run};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_on(listener, config(), FakeBackend::default()));
        let counter = Arc::new(AtomicUsize::new(0));
        let owner_factory: Arc<dyn Fn() -> TargetOwner + Send + Sync> = Arc::new(move || {
            let environment = format!("env-{}", counter.fetch_add(1, Ordering::SeqCst));
            owner("conformance", &environment)
        });
        let mut suite = ControllerConformance::new(
            format!("ws://{address}/control"),
            data_options(),
            TemplateId::new("small").unwrap(),
            owner_factory,
        );
        suite.poll_interval = Duration::from_millis(10);
        suite.settle_timeout = Duration::from_secs(10);
        let report = run(&suite).await;
        server.abort();
        report.into_result().expect("the controller conforms");
    }

    /// The same suite against a provider configured without a credential.
    #[tokio::test(flavor = "current_thread")]
    async fn the_controller_conforms_without_a_credential() {
        use environment_client::conformance::controller::{ControllerConformance, run};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_on(
            listener,
            config_with(19091, None),
            FakeBackend::default(),
        ));
        let counter = Arc::new(AtomicUsize::new(0));
        let owner_factory: Arc<dyn Fn() -> TargetOwner + Send + Sync> = Arc::new(move || {
            let environment = format!("env-{}", counter.fetch_add(1, Ordering::SeqCst));
            owner("conformance", &environment)
        });
        let mut suite = ControllerConformance::new(
            format!("ws://{address}/control"),
            ConnectOptions::new(ImplementationInfo::new("conformance").unwrap()),
            TemplateId::new("small").unwrap(),
            owner_factory,
        );
        suite.poll_interval = Duration::from_millis(10);
        suite.settle_timeout = Duration::from_secs(10);
        let report = run(&suite).await;
        server.abort();
        report.into_result().expect("the controller conforms");
    }
}
