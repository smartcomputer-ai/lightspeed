//! The provider's listener: the controller plane on `/control` and the data
//! routes under `/routes`, both behind the provider credential.
//!
//! Every target method answers promptly with the current observation. Slow
//! Incus work (an image pull, a boot, a graceful shutdown) runs as one
//! background job per target, serialized per target, and later calls observe
//! its progress: a running create reports `provisioning`, a running close
//! `closing`, and a failed create `failed` with its error as the `message`.
//! Job state is in memory; Incus itself, read through the deterministic owner
//! name, is the record of every target.

use std::{
    collections::{BTreeMap, HashMap},
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION,
    control::{
        handshake::{
            ControllerCapabilities, ControllerInitializeParams, ControllerInitializeResponse,
            ControllerRoutes,
        },
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            AdoptTargetParams, CloseTargetParams, CreateTargetParams, GetTargetParams,
            IngressObservation, PowerState, SetTargetPowerParams, TargetObservation, TargetOwner,
            TargetPhase, TargetRef, TargetResponse, TargetState,
        },
        template::{EnvironmentTemplate, ListTemplatesParams, ListTemplatesResponse},
    },
    error::{BusyReason, Capability, PreconditionReason, ProtocolError},
    handshake::{ImplementationInfo, Plane},
    ids::{BindingId, EnvironmentId, IncarnationId, ProviderTargetId, TemplateId, UniverseId},
    jsonrpc::{self, Request, Response as RpcResponse},
    limits::MAX_MESSAGE_BYTES,
    manifest::{
        self, ControllerInitialize, IngressEnsure, IngressRemove, MessageKind, TargetAdopt,
        TargetClose, TargetCreate, TargetGet, TargetSetPower, TemplateList,
    },
    metadata::{DescriptiveText, Metadata, validate_metadata_entry},
};
use futures_util::{FutureExt as _, SinkExt as _, StreamExt as _, future::BoxFuture};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, Semaphore, mpsc, watch};

use crate::{
    Config, IncusBackend,
    incus::{AdoptionSource, InstanceStatus, OwnedTarget, SUPPORTED_POWER_STATES},
    ingress, policy, relay,
};

/// How long a mutation waits for its background job before it answers with
/// the current observation.
const PROMPT_WAIT: Duration = Duration::from_secs(5);

/// Requests one controller connection may have in flight before `busy`.
const MAX_CONCURRENT_REQUESTS: usize = 32;

const _: () = assert!(
    MAX_CONCURRENT_REQUESTS >= environment_protocol::limits::MIN_CONCURRENT_REQUESTS as usize
);

#[derive(Clone)]
struct App<B> {
    config: Config,
    backend: B,
    work: Arc<Work>,
    initialize: Arc<ControllerInitializeResponse>,
}

impl<B: IncusBackend> App<B> {
    fn new(config: Config, backend: B) -> Self {
        let initialize = ControllerInitializeResponse {
            protocol_version: CONTROLLER_PROTOCOL_VERSION,
            plane: Plane::Controller,
            server_info: server_info(),
            capabilities: ControllerCapabilities {
                adopt_target: true,
                ingress: config.ingress.is_some(),
            },
            routes: ControllerRoutes {
                data: config.data_route_base.clone(),
            },
        };
        Self {
            config,
            backend,
            work: Arc::default(),
            initialize: Arc::new(initialize),
        }
    }
}

fn server_info() -> ImplementationInfo {
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

/// Serve on an already bound listener.
pub async fn serve_on<B: IncusBackend>(
    listener: tokio::net::TcpListener,
    config: Config,
    backend: B,
) -> anyhow::Result<()> {
    axum::serve(listener, router(config, backend)).await?;
    Ok(())
}

fn router<B: IncusBackend>(config: Config, backend: B) -> Router {
    Router::new()
        .route("/health", get(health::<B>))
        .route("/control", get(control_upgrade::<B>))
        .route(
            "/routes/{universe}/{binding}/{environment}/{incarnation}/{target}",
            get(data_upgrade::<B>),
        )
        .with_state(App::new(config, backend))
}

async fn health<B: IncusBackend>(State(app): State<App<B>>) -> Response {
    match app.backend.topology().await {
        Ok(topology) => Json(topology).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// True when the request carries the provider credential as a bearer token.
fn authorized(config: &Config, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| config.credential.matches(token))
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

async fn data_upgrade<B: IncusBackend>(
    State(app): State<App<B>>,
    Path((universe, binding, environment, incarnation, target_id)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !authorized(&app.config, &headers) {
        return unauthorized();
    }
    let owner = match (
        UniverseId::new(universe),
        BindingId::new(binding),
        EnvironmentId::new(environment),
        IncarnationId::new(incarnation),
    ) {
        (Ok(universe_id), Ok(binding_id), Ok(environment_id), Ok(incarnation_id)) => TargetOwner {
            universe_id,
            binding_id,
            environment_id,
            incarnation_id,
        },
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if target_id != policy::instance_name(&owner) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let target = match app
        .backend
        .get_owned(&policy::binding_of(&owner), &target_id)
        .await
    {
        Ok(Some(target)) if target.owner == owner => target,
        Ok(Some(_)) => return StatusCode::CONFLICT.into_response(),
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let guest = match relay::dial_guest(&app.config, &target).await {
        Ok(guest) => guest,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let config = app.config.clone();
    with_limits(upgrade)
        .on_upgrade(move |socket| relay::proxy(config, socket, guest))
        .into_response()
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
    let capabilities = &app.initialize.capabilities;
    match spec.capability {
        Some(Capability::AdoptTarget) if !capabilities.adopt_target => {
            return Err(ProtocolError::capability_unavailable(
                Capability::AdoptTarget,
                "this provider does not adopt targets",
            ));
        }
        Some(Capability::Ingress) if !capabilities.ingress => {
            return Err(ProtocolError::capability_unavailable(
                Capability::Ingress,
                "this provider has no public ingress configured",
            ));
        }
        _ => {}
    }
    match request.method.as_str() {
        TemplateList::NAME => encode(list_templates(app, request.params()?)),
        TargetCreate::NAME => encode(create_target(app, request.params()?).await?),
        TargetAdopt::NAME => encode(adopt_target(app, request.params()?).await?),
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
                public_ingress: template.public_ingress,
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
    observation(app, owner, &name, &observed, true)
        .await
        .map(|target| TargetResponse { target })
        .ok_or_else(|| ProtocolError::not_found(format!("no target exists for owner {name}")))
}

async fn create_target<B: IncusBackend>(
    app: &App<B>,
    params: CreateTargetParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = params.owner;
    let name = policy::instance_name(&owner);
    let observed = observe(app, &owner, &name).await?;
    if observed.holds_target() {
        // Idempotent per owner: an owner that holds a target gets it back,
        // and no creation policy runs again.
        return observed_response(app, &owner, &name, &observed).await;
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
    let backend = app.backend.clone();
    let job_owner = owner.clone();
    let job_name = name.clone();
    let job = async move {
        let binding = policy::binding_of(&job_owner);
        if backend.get_owned(&binding, &job_name).await?.is_some() {
            return Ok(());
        }
        backend.reconcile_binding(&binding).await?;
        backend.ensure_image(&template.image_fingerprint).await?;
        backend.create_vm(&job_owner, &template).await
    };
    let target = mutate(app, &owner, &name, WorkKind::Create, job.boxed())
        .await?
        .ok_or_else(|| ProtocolError::unavailable("the created Incus instance is not visible"))?;
    Ok(TargetResponse { target })
}

async fn adopt_target<B: IncusBackend>(
    app: &App<B>,
    params: AdoptTargetParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = params.owner;
    let source = params.source_target;
    let name = policy::instance_name(&owner);
    let observed = observe(app, &owner, &name).await?;
    let adopting_this = match (&observed.instance, &observed.slot) {
        (_, Some(slot)) if slot.running() && slot.kind == WorkKind::Close => true,
        (Some(instance), _) => instance.adoption_source.as_deref() == Some(source.as_str()),
        (None, Some(slot)) if slot.running() => slot.kind == WorkKind::Adopt,
        (None, _) => {
            crate::incus::parse_source_target(source.as_str())
                .map_err(|error| ProtocolError::invalid_field("sourceTarget", error.to_string()))?;
            match app
                .backend
                .adoption_source(&source)
                .await
                .map_err(unavailable)?
            {
                AdoptionSource::Adoptable => {}
                AdoptionSource::Missing => {
                    return Err(ProtocolError::not_found(format!(
                        "adoption source {source} does not exist"
                    )));
                }
                AdoptionSource::Managed => {
                    return Err(ProtocolError::failed_precondition(
                        PreconditionReason::OwnerMismatch,
                        format!("adoption source {source} already belongs to an owner"),
                    ));
                }
                AdoptionSource::NotAdoptable(reason) => {
                    return Err(ProtocolError::failed_precondition(
                        PreconditionReason::NotAdoptable,
                        format!("adoption source {source}: {reason}"),
                    ));
                }
            }
            let backend = app.backend.clone();
            let job_owner = owner.clone();
            let job_name = name.clone();
            let job = async move {
                let binding = policy::binding_of(&job_owner);
                if backend.get_owned(&binding, &job_name).await?.is_some() {
                    return Ok(());
                }
                backend.reconcile_binding(&binding).await?;
                backend.adopt_vm(&job_owner, &source).await
            };
            let target = mutate(app, &owner, &name, WorkKind::Adopt, job.boxed())
                .await?
                .ok_or_else(|| {
                    ProtocolError::unavailable("the adopted Incus instance is not visible")
                })?;
            return Ok(TargetResponse { target });
        }
    };
    if !adopting_this {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::OwnerMismatch,
            "the owner already holds a different target",
        ));
    }
    observed_response(app, &owner, &name, &observed).await
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
    let current = observation(app, owner, &name, &observed, false)
        .await
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} does not exist")))?;
    if current.state.phase != TargetPhase::Active {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::TargetNotActive,
            format!("target {name} is {:?}, not active", current.state.phase),
        ));
    }
    if current.state.power == Some(power) && !observed.slot.as_ref().is_some_and(Slot::running) {
        return observed_response(app, owner, &name, &observed).await;
    }
    let backend = app.backend.clone();
    let binding = policy::binding_of(owner);
    let job_name = name.clone();
    let job = async move {
        let target = backend
            .get_owned(&binding, &job_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the Incus instance disappeared"))?;
        backend.set_power(&target, power).await
    };
    let target = mutate(app, owner, &name, WorkKind::Power(power), job.boxed())
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
    let backend = app.backend.clone();
    let binding = policy::binding_of(owner);
    let job_name = name.clone();
    let job = async move {
        match backend.get_owned(&binding, &job_name).await? {
            Some(target) => backend.delete_vm(&target).await,
            None => Ok(()),
        }
    };
    app.work.start(&name, WorkKind::Close, job.boxed());
    wait_for_work(app, &name).await;
    let observed = observe(app, owner, &name).await?;
    if !observed.holds_target() {
        return Ok(TargetResponse {
            target: closed(owner),
        });
    }
    let mut target = observation(app, owner, &name, &observed, false)
        .await
        .unwrap_or_else(|| closed(owner));
    // Until the instance is gone, a close answers closing, with the error of
    // a failed attempt as its message; the next close tries again.
    target.state = TargetState {
        phase: TargetPhase::Closing,
        power: None,
        daemon_ready: false,
        message: target.state.message,
    };
    target.supported_power_states.clear();
    Ok(TargetResponse { target })
}

async fn ensure_ingress<B: IncusBackend>(
    app: &App<B>,
    params: EnsureIngressParams,
) -> Result<TargetResponse, ProtocolError> {
    let owner = &params.target.owner;
    let name = resolve(app, &params.target).await?;
    let observed = observe(app, owner, &name).await?;
    let Some(instance) = ingress_subject(app, owner, &name, &observed).await? else {
        return observed_response(app, owner, &name, &observed).await;
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
        return observed_response(app, owner, &name, &observed).await;
    }
    let backend = app.backend.clone();
    let binding = policy::binding_of(owner);
    let job_name = name.clone();
    let job = async move {
        let target = backend
            .get_owned(&binding, &job_name)
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
    let Some(instance) = ingress_subject(app, owner, &name, &observed).await? else {
        return observed_response(app, owner, &name, &observed).await;
    };
    if instance.ingress_hostname.is_none() && instance.ingress_port.is_none() {
        return observed_response(app, owner, &name, &observed).await;
    }
    let backend = app.backend.clone();
    let binding = policy::binding_of(owner);
    let job_name = name.clone();
    let job = async move {
        let target = backend
            .get_owned(&binding, &job_name)
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
async fn ingress_subject<B: IncusBackend>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    observed: &Observed,
) -> Result<Option<OwnedTarget>, ProtocolError> {
    let current = observation(app, owner, name, observed, false)
        .await
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} does not exist")))?;
    if current.state.phase == TargetPhase::Closing {
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
                .get_owned(&policy::binding_of(&target.owner), target_id.as_str())
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
        .get_owned(&policy::binding_of(owner), name)
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

async fn observed_response<B: IncusBackend>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    observed: &Observed,
) -> Result<TargetResponse, ProtocolError> {
    observation(app, owner, name, observed, true)
        .await
        .map(|target| TargetResponse { target })
        .ok_or_else(|| ProtocolError::not_found(format!("target {name} does not exist")))
}

/// The observation of a target, or `None` when the owner holds nothing.
/// `probe` runs the envd liveness probe for a running instance.
///
/// `targetId` is reported only once the Incus instance exists. Until then
/// the runtime keeps sending the idempotent `target/create` (or
/// `target/adopt`), which starts the job again when it failed or was lost
/// with a provider restart; a failed attempt is reported as `provisioning`
/// with its error as `message`, so the runtime retries it until its
/// provisioning deadline instead of failing at the first Incus hiccup.
async fn observation<B: IncusBackend>(
    app: &App<B>,
    owner: &TargetOwner,
    name: &str,
    observed: &Observed,
    probe: bool,
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
        .filter(|slot| !slot.running())
        .and_then(|slot| match slot.kind {
            WorkKind::Create => Some("target/create"),
            WorkKind::Adopt => Some("target/adopt"),
            _ => None,
        });
    let state = match (running, &observed.instance) {
        (Some(WorkKind::Close), _) => {
            phase_state(TargetPhase::Closing, None, "closing the Incus instance")
        }
        (Some(WorkKind::Create), _) => phase_state(
            TargetPhase::Provisioning,
            None,
            "creating and starting the Incus instance",
        ),
        (Some(WorkKind::Adopt), _) => phase_state(
            TargetPhase::Provisioning,
            None,
            "adopting the Incus instance",
        ),
        (_, Some(instance)) => {
            let mut state = instance_state(instance);
            if probe && instance.status == InstanceStatus::Running {
                state.daemon_ready = relay::probe_guest(&app.config, instance).await;
            }
            if state.message.is_none() {
                state.message = failure.and_then(DescriptiveText::lossy);
            }
            state
        }
        (_, None) => {
            let retried_by = failed_creation?;
            phase_state(
                TargetPhase::Provisioning,
                None,
                &format!(
                    "the last attempt failed and the next {retried_by} retries it: {}",
                    failure.unwrap_or("unknown error")
                ),
            )
        }
    };
    let target_id = match &observed.instance {
        Some(_) => Some(ProviderTargetId::new(name).ok()?),
        None => None,
    };
    let instance = observed.instance.as_ref();
    let ingress = instance
        .and_then(|instance| instance.ingress_hostname.as_deref())
        .and_then(|hostname| ingress::public_endpoint(&app.config, hostname).ok())
        .map(|endpoint| IngressObservation { endpoint });
    let metadata = instance.map_or_else(Metadata::default, |instance| {
        display_metadata([
            ("templateId", instance.template_id.as_str()),
            ("imageFingerprint", instance.image_fingerprint.as_str()),
            (
                "incusMember",
                instance.location.as_deref().unwrap_or_default(),
            ),
        ])
    });
    let supported_power_states = if state.phase == TargetPhase::Closing {
        Vec::new()
    } else {
        SUPPORTED_POWER_STATES.to_vec()
    };
    Some(TargetObservation {
        target_id,
        owner: owner.clone(),
        display_name: None,
        state,
        supported_power_states,
        ingress,
        metadata,
    })
}

fn phase_state(phase: TargetPhase, power: Option<PowerState>, message: &str) -> TargetState {
    TargetState {
        phase,
        power,
        daemon_ready: false,
        message: DescriptiveText::lossy(message),
    }
}

/// The two-axis state an Incus instance status maps to. A starting VM is
/// reported running and a stopping one stopped: `power` names where the
/// target is going, and `daemonReady` says whether envd answers yet.
fn instance_state(instance: &OwnedTarget) -> TargetState {
    let active = |power| TargetState {
        phase: TargetPhase::Active,
        power: Some(power),
        daemon_ready: false,
        message: None,
    };
    match instance.status {
        InstanceStatus::Running | InstanceStatus::Starting => active(PowerState::Running),
        InstanceStatus::Frozen => active(PowerState::Paused),
        InstanceStatus::Stopped | InstanceStatus::Stopping => active(PowerState::Stopped),
        InstanceStatus::Error => phase_state(
            TargetPhase::Failed,
            None,
            "Incus reports the instance in the Error state",
        ),
        InstanceStatus::Other => phase_state(
            TargetPhase::Unknown,
            None,
            "Incus reports an instance status this provider does not recognize",
        ),
    }
}

fn closed(owner: &TargetOwner) -> TargetObservation {
    TargetObservation {
        target_id: None,
        owner: owner.clone(),
        display_name: None,
        state: TargetState {
            phase: TargetPhase::Closed,
            power: None,
            daemon_ready: false,
            message: None,
        },
        supported_power_states: Vec::new(),
        ingress: None,
        metadata: Metadata::default(),
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
    Ok(observation(app, owner, name, &observed, true).await)
}

async fn wait_for_work<B>(app: &App<B>, name: &str) {
    if let Some(mut done) = app.work.done(name) {
        let _ = tokio::time::timeout(PROMPT_WAIT, done.wait_for(|finished| *finished)).await;
    }
}

/// The background job kinds; at most one job per target is current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkKind {
    Create,
    Adopt,
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
struct Work {
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
    fn start(
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

    fn done(&self, name: &str) -> Option<watch::Receiver<bool>> {
        self.lock()
            .slots
            .get(name)
            .filter(|slot| slot.running())
            .map(|slot| slot.done.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use environment_protocol::{
        control::target::BindingRef,
        error::{ErrorData, ProtocolErrorKind},
        ids::ProviderNativeRef,
        jsonrpc::Id,
        routes::DataRouteBase,
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

    const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef";

    /// An in-memory Incus: instances by name, every mutation instant.
    #[derive(Clone, Default)]
    struct FakeBackend {
        instances: Arc<StdMutex<BTreeMap<String, OwnedTarget>>>,
        creates: Arc<AtomicUsize>,
        deletes: Arc<AtomicUsize>,
    }

    impl FakeBackend {
        fn insert(&self, owner: &TargetOwner, template_id: &str, status: InstanceStatus) {
            let name = policy::instance_name(owner);
            self.instances.lock().unwrap().insert(
                name.clone(),
                OwnedTarget {
                    target_id: ProviderTargetId::new(name).unwrap(),
                    owner: owner.clone(),
                    template_id: template_id.to_owned(),
                    image_fingerprint: "fingerprint".to_owned(),
                    adoption_source: None,
                    status,
                    ipv4_address: None,
                    ingress_hostname: None,
                    ingress_port: None,
                    location: None,
                },
            );
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

        async fn reconcile_binding(&self, _binding: &BindingRef) -> anyhow::Result<()> {
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
            binding: &BindingRef,
            name: &str,
        ) -> anyhow::Result<Option<OwnedTarget>> {
            Ok(self
                .instances
                .lock()
                .unwrap()
                .get(name)
                .filter(|target| target.binding() == *binding)
                .cloned())
        }

        async fn create_vm(
            &self,
            owner: &TargetOwner,
            template: &TemplatePolicy,
        ) -> anyhow::Result<()> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.insert(
                owner,
                template.template_id.as_str(),
                InstanceStatus::Running,
            );
            Ok(())
        }

        async fn adoption_source(
            &self,
            _source: &ProviderNativeRef,
        ) -> anyhow::Result<AdoptionSource> {
            Ok(AdoptionSource::Missing)
        }

        async fn adopt_vm(
            &self,
            _owner: &TargetOwner,
            _source: &ProviderNativeRef,
        ) -> anyhow::Result<()> {
            anyhow::bail!("the fake backend adopts nothing")
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

    fn template(id: &str, deprecated: bool) -> TemplatePolicy {
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
            resources: Default::default(),
        }
    }

    fn config() -> Config {
        let templates = [template("small", false), template("retired", true)]
            .into_iter()
            .map(|template| (template.template_id.clone(), template))
            .collect();
        Config(Arc::new(ConfigInner {
            controller_listen: "127.0.0.1:0".parse().unwrap(),
            credential: ProviderCredential::new(CREDENTIAL).unwrap(),
            data_route_base: DataRouteBase::new("ws://127.0.0.1:19090/routes").unwrap(),
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
            envd_port: 19091,
            ingress: None,
        }))
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
        let request = Request::new(Id::Number(1), "initialize", &params(4)).unwrap();
        let mismatch = initialize(&app, &mut initialized, &request)
            .result
            .unwrap_err();
        assert_eq!(
            mismatch.data,
            ErrorData::VersionMismatch {
                expected: CONTROLLER_PROTOCOL_VERSION,
                received: 4
            }
        );
        assert!(!initialized);

        let request = Request::new(Id::Number(2), "initialize", &params(3)).unwrap();
        let response: ControllerInitializeResponse = initialize(&app, &mut initialized, &request)
            .decode()
            .unwrap();
        assert!(initialized);
        assert_eq!(response.plane, Plane::Controller);
        assert!(response.capabilities.adopt_target);
        assert!(!response.capabilities.ingress);
        assert_eq!(response.routes.data.as_str(), "ws://127.0.0.1:19090/routes");

        let repeat = initialize(&app, &mut initialized, &request)
            .result
            .unwrap_err();
        assert_eq!(repeat.kind(), ProtocolErrorKind::InvalidRequest);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_and_unadvertised_methods_are_typed_errors() {
        let (app, _) = app();
        let unknown = call(&app, "controller/listTargets", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(unknown.kind(), ProtocolErrorKind::MethodNotFound);
        let ingress = call(
            &app,
            IngressEnsure::NAME,
            &EnsureIngressParams {
                target: by_owner(&owner("u", "b", "e")),
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
        let owner = owner("u", "b", "e");
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
        let owner = owner("u", "b", "e");
        let params = CreateTargetParams {
            owner: owner.clone(),
            template_id: TemplateId::new("small").unwrap(),
        };
        let first = call(&app, TargetCreate::NAME, &params).await.unwrap();
        let second = call(&app, TargetCreate::NAME, &params).await.unwrap();
        assert_eq!(first.owner, owner);
        assert_eq!(first.target_id, second.target_id);
        assert_eq!(
            first.target_id.as_ref().map(ProviderTargetId::as_str),
            Some(policy::instance_name(&owner).as_str())
        );
        assert_eq!(second.state.phase, TargetPhase::Active);
        assert_eq!(second.state.power, Some(PowerState::Running));
        assert_eq!(backend.creates.load(Ordering::SeqCst), 1);

        // A target that already exists comes back even when its template
        // has been deprecated since: observation applies no creation policy.
        let existing = crate::policy::tests::owner("u", "b", "old");
        backend.insert(&existing, "retired", InstanceStatus::Stopped);
        let observed = call(
            &app,
            TargetCreate::NAME,
            &CreateTargetParams {
                owner: existing.clone(),
                template_id: TemplateId::new("retired").unwrap(),
            },
        )
        .await
        .unwrap();
        assert_eq!(observed.state.power, Some(PowerState::Stopped));
        assert_eq!(backend.creates.load(Ordering::SeqCst), 1);

        let refused = call(
            &app,
            TargetCreate::NAME,
            &CreateTargetParams {
                owner: crate::policy::tests::owner("u", "b", "new"),
                template_id: TemplateId::new("retired").unwrap(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            reason(&refused),
            Some(PreconditionReason::TemplateDeprecated)
        );
        let missing = call(
            &app,
            TargetCreate::NAME,
            &CreateTargetParams {
                owner: crate::policy::tests::owner("u", "b", "new"),
                template_id: TemplateId::new("absent").unwrap(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(missing.kind(), ProtocolErrorKind::NotFound);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_foreign_target_id_is_an_owner_mismatch() {
        let (app, backend) = app();
        let holder = owner("u", "b", "holder");
        backend.insert(&holder, "small", InstanceStatus::Running);
        let error = call(
            &app,
            TargetGet::NAME,
            &GetTargetParams {
                target: TargetRef {
                    owner: owner("u", "b", "other"),
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
        let owner = owner("u", "b", "e");
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
        assert_eq!(closed.state.phase, TargetPhase::Closed);
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
        assert_eq!(again.state.phase, TargetPhase::Closed);
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
        let owner = owner("u", "b", "e");
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
        assert_eq!(paused.state.power, Some(PowerState::Paused));

        backend.insert(&owner, "small", InstanceStatus::Error);
        let failed = call(&app, TargetSetPower::NAME, &set(PowerState::Running))
            .await
            .unwrap_err();
        assert_eq!(reason(&failed), Some(PreconditionReason::TargetNotActive));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_create_is_provisioning_without_an_id_and_retried_by_create() {
        let (app, backend) = app();
        let owner = owner("u", "b", "e");
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
        assert_eq!(observed.state.phase, TargetPhase::Provisioning);
        assert_eq!(observed.target_id, None);
        assert!(
            observed
                .state
                .message
                .as_ref()
                .is_some_and(|message| message.as_str().contains("image server unreachable"))
        );
        let created = call(
            &app,
            TargetCreate::NAME,
            &CreateTargetParams {
                owner: owner.clone(),
                template_id: TemplateId::new("small").unwrap(),
            },
        )
        .await
        .unwrap();
        assert_eq!(created.state.phase, TargetPhase::Active);
        assert_eq!(
            created.target_id.as_ref().map(ProviderTargetId::as_str),
            Some(name.as_str())
        );
        assert_eq!(backend.creates.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_an_owner_whose_create_failed_forgets_the_failure() {
        let (app, _) = app();
        let owner = owner("u", "b", "e");
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
        assert_eq!(closed.state.phase, TargetPhase::Closed);
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

    /// The client's controller conformance suite against this server over
    /// a real WebSocket, with the in-memory backend standing in for Incus.
    #[tokio::test(flavor = "current_thread")]
    async fn the_controller_passes_the_conformance_suite() {
        use environment_client::{
            ConnectOptions,
            conformance::controller::{ControllerConformance, run},
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_on(listener, config(), FakeBackend::default()));
        let options = ConnectOptions::new(ImplementationInfo::new("conformance").unwrap())
            .with_bearer_token(CREDENTIAL);
        let counter = Arc::new(AtomicUsize::new(0));
        let owner_factory: Arc<dyn Fn() -> TargetOwner + Send + Sync> = Arc::new(move || {
            let environment = format!("env-{}", counter.fetch_add(1, Ordering::SeqCst));
            owner("conformance", "fake", &environment)
        });
        let mut suite = ControllerConformance::new(
            format!("ws://{address}/control"),
            options,
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
