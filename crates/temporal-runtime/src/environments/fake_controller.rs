//! An in-memory provider controller for tests.
//!
//! It serves the controller plane on `/control` and a minimal data plane on
//! `/routes/{universeId}/{environmentId}/{targetId}` over a loopback
//! WebSocket listener, so Lightspeed's derived data route base
//! (`{controller origin}/routes`) reaches it. When started with a
//! credential it checks it as a bearer token on both, exactly as a real
//! provider must; without one it checks nothing. Nothing registers it: a
//! test starts one and registers its controller URL (and credential, if
//! any) like any provider's.
//!
//! Targets come up at once: `target/create` answers an `active`, `running`
//! target, and power changes and closes complete before they answer. A data
//! route dial wakes a powered-down target and refuses `waking` once, and a
//! new target may refuse `booting` for a configured number of dials; after
//! that the route answers `initialize` only, as an envd with no capability
//! families, which is enough for the runtime's open handshake. Tests change
//! targets out of band the way a provider's own idle policy or TTL would.

use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION, ENVD_PROTOCOL_VERSION,
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
        template::{EnvironmentTemplate, ListTemplatesResponse, TemplateResources},
    },
    data::handshake::{
        DataCapabilities, DataLimits, FilesystemLimits, GlobCeilings, InitializeParams,
        InitializeResponse, JobLimits, ProcessLimits, ScanCeilings, SearchCeilings,
        TransferCeilings,
    },
    error::{Capability, PreconditionReason, ProtocolError, ProtocolErrorKind},
    handshake::{ImplementationInfo, Plane},
    ids::{DaemonInstanceId, EnvironmentId, ProviderTargetId, TemplateId, UniverseId},
    jsonrpc::{self, Request, Response as RpcResponse},
    limits::{MAX_MESSAGE_BYTES, MIN_CONCURRENT_REQUESTS},
    manifest::{
        ControllerInitialize, IngressEnsure, IngressRemove, RequestMethod, TargetClose,
        TargetCreate, TargetGet, TargetSetPower, TemplateList,
    },
    metadata::{DescriptiveText, DisplayName, Metadata},
    paths::EnvironmentPath,
    routes::{DataRouteRefusal, PublicUrl},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

/// The one template the fake controller offers.
pub const FAKE_TEMPLATE_ID: &str = "rust-v1";

/// Public endpoint the fake controller reports for enabled ingress.
pub const FAKE_INGRESS_ENDPOINT: &str = "https://fake.env.test";

/// Power states the fake controller supports when it offers power control.
/// Suspended is included so the suspend path has coverage.
pub const FAKE_POWER_STATES: [PowerState; 4] = [
    PowerState::Running,
    PowerState::Paused,
    PowerState::Suspended,
    PowerState::Stopped,
];

/// `Retry-After` of the fake data route's 503 refusals, in seconds.
pub const FAKE_RETRY_AFTER_SECS: u64 = 1;

/// What the fake controller advertises and how its data routes behave.
#[derive(Clone, Debug)]
pub struct FakeControllerOptions {
    /// `capabilities.power`.
    pub power: bool,
    /// `capabilities.ingress`.
    pub ingress: bool,
    /// Require a bearer credential on the controller and its data routes.
    pub credential: bool,
    /// Data route dials of a new target that are refused `booting` before
    /// its envd answers.
    pub booting_dials: u32,
    /// The protocol version the fake envd behind every data route speaks.
    pub daemon_protocol_version: u32,
}

impl Default for FakeControllerOptions {
    fn default() -> Self {
        Self {
            power: true,
            ingress: true,
            credential: true,
            booting_dials: 0,
            daemon_protocol_version: ENVD_PROTOCOL_VERSION,
        }
    }
}

/// A running fake controller. Dropping it stops the listener.
pub struct FakeController {
    controller_url: String,
    credential: Option<String>,
    app: Arc<App>,
    task: JoinHandle<()>,
}

impl FakeController {
    /// Serve a fake controller with the default options on a loopback port.
    pub async fn start() -> std::io::Result<Self> {
        Self::start_with(FakeControllerOptions::default()).await
    }

    pub async fn start_with(options: FakeControllerOptions) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let credential = options
            .credential
            .then(|| format!("fake-credential-{}", uuid::Uuid::new_v4().simple()));
        let initialize = ControllerInitializeResponse {
            protocol_version: CONTROLLER_PROTOCOL_VERSION,
            plane: Plane::Controller,
            server_info: implementation("lightspeed-fake-controller"),
            capabilities: ControllerCapabilities {
                power: options.power,
                ingress: options.ingress,
            },
        };
        let app = Arc::new(App {
            credential: credential.clone(),
            options,
            initialize,
            reachable: AtomicBool::new(true),
            targets: Mutex::new(Targets::default()),
        });
        let router = Router::new()
            .route("/control", get(control_upgrade))
            .route(
                "/routes/{universe}/{environment}/{target}",
                get(data_upgrade),
            )
            .with_state(app.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            controller_url: format!("ws://{address}/control"),
            credential,
            app,
            task,
        })
    }

    /// The controller URL to register the provider with.
    pub fn controller_url(&self) -> &str {
        &self.controller_url
    }

    /// The provider credential the controller and its data routes accept;
    /// `None` when they check none.
    pub fn credential(&self) -> Option<&str> {
        self.credential.as_deref()
    }

    /// While `false`, the controller and every data route answer 503
    /// without upgrading, as a provider that is down would.
    pub fn set_reachable(&self, reachable: bool) {
        self.app.reachable.store(reachable, Ordering::SeqCst);
    }

    /// Move the environment's target to `power` out of band, as the
    /// provider's own idle policy would. False when it holds no target.
    pub async fn set_power_out_of_band(&self, environment_id: &str, power: PowerState) -> bool {
        let mut targets = self.app.targets.lock().await;
        match targets.find_mut(environment_id) {
            Some(target) => {
                target.power = power;
                true
            }
            None => false,
        }
    }

    /// Remove the environment's target out of band, as a provider TTL
    /// would. False when it holds no target.
    pub async fn end_target(&self, environment_id: &str) -> bool {
        let mut targets = self.app.targets.lock().await;
        let Some(target_id) = targets
            .find_mut(environment_id)
            .map(|target| target.target_id.clone())
        else {
            return false;
        };
        targets.remove(&target_id);
        true
    }

    /// How many targets the controller holds.
    pub async fn target_count(&self) -> usize {
        self.app.targets.lock().await.by_id.len()
    }
}

impl Drop for FakeController {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct App {
    credential: Option<String>,
    options: FakeControllerOptions,
    initialize: ControllerInitializeResponse,
    reachable: AtomicBool,
    targets: Mutex<Targets>,
}

#[derive(Default)]
struct Targets {
    by_id: BTreeMap<ProviderTargetId, FakeTarget>,
    by_owner: BTreeMap<TargetOwner, ProviderTargetId>,
}

#[derive(Clone)]
struct FakeTarget {
    target_id: ProviderTargetId,
    owner: TargetOwner,
    power: PowerState,
    ingress: bool,
    booting_dials: u32,
}

impl FakeTarget {
    fn observation(&self) -> TargetObservation {
        TargetObservation {
            target_id: Some(self.target_id.clone()),
            owner: self.owner.clone(),
            phase: TargetPhase::Active,
            power: Some(self.power),
            message: None,
            ingress: self.ingress.then(|| IngressObservation {
                endpoint: PublicUrl::new(FAKE_INGRESS_ENDPOINT).expect("valid fake endpoint"),
            }),
        }
    }
}

fn implementation(name: &str) -> ImplementationInfo {
    ImplementationInfo::new(name)
        .and_then(|info| info.with_version(env!("CARGO_PKG_VERSION")))
        .expect("fake implementation info is within bounds")
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    let Some(credential) = &app.credential else {
        return true;
    };
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), credential.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

/// The answer before any upgrade: 503 while unreachable, 401 without the
/// credential.
fn gate(app: &App, headers: &HeaderMap) -> Option<Response> {
    if !app.reachable.load(Ordering::SeqCst) {
        return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
    }
    if !authorized(app, headers) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
            )
                .into_response(),
        );
    }
    None
}

/// A typed data route refusal: its status, a JSON body and, for a 503,
/// `Retry-After`.
fn refuse(refusal: DataRouteRefusal) -> Response {
    let status = StatusCode::from_u16(refusal.status()).expect("refusal statuses are valid");
    let body = serde_json::to_string(&refusal).expect("refusals encode");
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/json".to_owned())],
        body,
    )
        .into_response();
    if refusal.is_retryable() {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            FAKE_RETRY_AFTER_SECS
                .to_string()
                .parse()
                .expect("valid header"),
        );
    }
    response
}

fn with_limits(upgrade: WebSocketUpgrade) -> WebSocketUpgrade {
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES as usize)
        .max_frame_size(MAX_MESSAGE_BYTES as usize)
}

async fn control_upgrade(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Some(refused) = gate(&app, &headers) {
        return refused;
    }
    with_limits(upgrade)
        .on_upgrade(move |socket| serve(socket, Plane::Controller, app))
        .into_response()
}

async fn data_upgrade(
    State(app): State<Arc<App>>,
    Path((universe, environment, target_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Some(refused) = gate(&app, &headers) {
        return refused;
    }
    let (Ok(universe_id), Ok(environment_id), Ok(target_id)) = (
        UniverseId::new(universe),
        EnvironmentId::new(environment),
        ProviderTargetId::new(target_id),
    ) else {
        return refuse(DataRouteRefusal::Gone {});
    };
    let owner = TargetOwner {
        universe_id,
        environment_id,
    };
    {
        let mut targets = app.targets.lock().await;
        let Some(target) = targets.by_id.get_mut(&target_id) else {
            return refuse(DataRouteRefusal::Gone {});
        };
        if target.owner != owner {
            return refuse(DataRouteRefusal::Gone {});
        }
        // The dial is the wake signal.
        if target.power != PowerState::Running {
            target.power = PowerState::Running;
            return refuse(DataRouteRefusal::Waking {});
        }
        if target.booting_dials > 0 {
            target.booting_dials -= 1;
            return refuse(DataRouteRefusal::Booting {});
        }
    }
    with_limits(upgrade)
        .on_upgrade(move |socket| serve(socket, Plane::Data, app))
        .into_response()
}

/// One connection: the envelope and the `initialize`-first handshake,
/// requests answered in order.
async fn serve(socket: WebSocket, plane: Plane, app: Arc<App>) {
    let (mut sink, mut stream) = socket.split();
    let mut initialized = false;
    while let Some(Ok(frame)) = stream.next().await {
        let reply = match frame {
            Message::Text(text) => match jsonrpc::parse(text.as_str()) {
                Err(rejection) => rejection.into_response(),
                Ok(jsonrpc::Message::Request(request)) => {
                    Some(if request.method == ControllerInitialize::NAME {
                        initialize(&app, plane, &mut initialized, &request)
                    } else if !initialized {
                        RpcResponse::failure(Some(request.id), ProtocolError::not_initialized())
                    } else {
                        let result = match plane {
                            Plane::Controller => controller_call(&app, &request).await,
                            _ => Err(ProtocolError::method_not_found(&request.method)),
                        };
                        RpcResponse {
                            id: Some(request.id),
                            result,
                        }
                    })
                }
                Ok(jsonrpc::Message::Notification(_) | jsonrpc::Message::Response(_)) => None,
            },
            Message::Binary(_) => Some(RpcResponse::failure(
                None,
                ProtocolError::invalid_request("binary frames are not part of any plane"),
            )),
            Message::Ping(payload) => {
                if sink.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
                None
            }
            Message::Pong(_) => None,
            Message::Close(_) => break,
        };
        if let Some(reply) = reply
            && sink
                .send(Message::Text(reply.to_text().into()))
                .await
                .is_err()
        {
            break;
        }
    }
    let _ = sink.close().await;
}

fn initialize(app: &App, plane: Plane, initialized: &mut bool, request: &Request) -> RpcResponse {
    let id = Some(request.id.clone());
    if *initialized {
        return RpcResponse::failure(
            id,
            ProtocolError::invalid_request("initialize was already called on this connection"),
        );
    }
    let expected = match plane {
        Plane::Controller => CONTROLLER_PROTOCOL_VERSION,
        _ => app.options.daemon_protocol_version,
    };
    if let Some(version) = request
        .params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        && version != u64::from(expected)
    {
        return RpcResponse::failure(
            id,
            ProtocolError::version_mismatch(expected, u32::try_from(version).unwrap_or(u32::MAX)),
        );
    }
    let result = match plane {
        Plane::Controller => request
            .params::<ControllerInitializeParams>()
            .and_then(|_| encode(&app.initialize)),
        _ => request
            .params::<InitializeParams>()
            .and_then(|_| encode(&data_initialize(expected))),
    };
    if result.is_ok() {
        *initialized = true;
    }
    RpcResponse { id, result }
}

/// The fake envd's handshake: no capability families, so a client that
/// needs one is refused before calling anything.
fn data_initialize(protocol_version: u32) -> InitializeResponse {
    InitializeResponse {
        protocol_version,
        plane: Plane::Data,
        daemon_instance_id: DaemonInstanceId::new(format!(
            "envd_{}",
            uuid::Uuid::new_v4().simple()
        ))
        .expect("valid daemon instance id"),
        server_info: implementation("lightspeed-fake-envd"),
        capabilities: DataCapabilities::default(),
        limits: DataLimits {
            max_concurrent_requests: MIN_CONCURRENT_REQUESTS,
            max_wait_ms: 60_000,
            process: ProcessLimits {
                max_read_bytes: 1 << 20,
                max_retained_output_bytes: 1 << 20,
                retention_after_exit_ms: 60_000,
                unread_retention_ms: 60_000,
                max_exited_retained_bytes: 1 << 20,
                terminate_grace_ms: 1_000,
                max_input_queue_bytes: 1 << 20,
            },
            job: JobLimits {
                max_read_bytes: 1 << 20,
                max_request_jobs: 1,
                max_retained_output_bytes: 1 << 20,
                record_retention_ms: 60_000,
                cancel_grace_ms: 1_000,
            },
            filesystem: FilesystemLimits {
                max_read_file_bytes: 1 << 20,
                max_write_file_bytes: 1 << 20,
                max_directory_entries: 1,
                search: SearchCeilings {
                    max_matches: 1,
                    max_files: 1,
                    max_bytes: 1,
                    max_duration_ms: 1_000,
                    max_line_bytes: 1,
                },
                glob: GlobCeilings {
                    max_matches: 1,
                    max_files: 1,
                    max_duration_ms: 1_000,
                },
                scan: ScanCeilings {
                    max_roots: 1,
                    max_patterns: 1,
                    max_entries: 1,
                    max_file_bytes: 1,
                    max_total_bytes: 1,
                    max_duration_ms: 1_000,
                },
                transfer: TransferCeilings {
                    max_active_operations: 1,
                    max_content_chunk_bytes: 1,
                    max_inventory_page_entries: 1,
                    receipt_retention_ms: 60_000,
                    max_entries: 1,
                    max_depth: 1,
                    max_file_bytes: 1,
                    max_total_bytes: 1,
                    max_manifest_bytes: 1,
                    max_duration_ms: 1_000,
                },
            },
        },
        default_cwd: EnvironmentPath::new("/").expect("root path"),
        home_directory: None,
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Value, ProtocolError> {
    serde_json::to_value(value).map_err(|error| ProtocolError::internal(error.to_string()))
}

fn params<M: RequestMethod>(request: &Request) -> Result<M::Params, ProtocolError>
where
    M::Params: DeserializeOwned,
{
    request.params::<M::Params>()
}

async fn controller_call(app: &App, request: &Request) -> Result<Value, ProtocolError> {
    let method = request.method.as_str();
    if method == TemplateList::NAME {
        params::<TemplateList>(request)?;
        return encode(&list_templates(&app.options));
    }
    if (method == IngressEnsure::NAME || method == IngressRemove::NAME) && !app.options.ingress {
        return Err(ProtocolError::capability_unavailable(
            Capability::Ingress,
            "the fake controller was started without ingress",
        ));
    }
    if method == TargetSetPower::NAME && !app.options.power {
        return Err(ProtocolError::capability_unavailable(
            Capability::Power,
            "the fake controller was started without power control",
        ));
    }
    let mut targets = app.targets.lock().await;
    let observation = match method {
        TargetCreate::NAME => {
            targets.create(params::<TargetCreate>(request)?, app.options.booting_dials)?
        }
        TargetGet::NAME => {
            let GetTargetParams { target } = params::<TargetGet>(request)?;
            targets.resolve(&target)?.observation()
        }
        TargetSetPower::NAME => targets.set_power(params::<TargetSetPower>(request)?)?,
        TargetClose::NAME => targets.close(params::<TargetClose>(request)?)?,
        IngressEnsure::NAME => {
            let EnsureIngressParams { target } = params::<IngressEnsure>(request)?;
            targets.set_ingress(&target, true)?
        }
        IngressRemove::NAME => {
            let RemoveIngressParams { target } = params::<IngressRemove>(request)?;
            targets.set_ingress(&target, false)?
        }
        other => return Err(ProtocolError::method_not_found(other)),
    };
    encode(&TargetResponse {
        target: observation,
    })
}

fn list_templates(options: &FakeControllerOptions) -> ListTemplatesResponse {
    ListTemplatesResponse {
        templates: vec![EnvironmentTemplate {
            template_id: TemplateId::new(FAKE_TEMPLATE_ID).expect("valid template id"),
            display_name: DisplayName::new("Rust development").expect("valid display name"),
            description: DescriptiveText::new("Deterministic fake provider template").ok(),
            power_states: if options.power {
                FAKE_POWER_STATES.to_vec()
            } else {
                Vec::new()
            },
            public_ingress: options.ingress,
            idle: None,
            deprecated: false,
            resources: Some(TemplateResources {
                vcpus: Some(2),
                memory_bytes: Some(4 << 30),
                disk_bytes: Some(20 << 30),
            }),
            metadata: Metadata::default(),
        }],
    }
}

fn owner_mismatch() -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::OwnerMismatch,
        "the target belongs to another owner",
    )
}

impl Targets {
    /// The owner's target, or the named one after the ownership fence.
    fn resolve(&self, target: &TargetRef) -> Result<&FakeTarget, ProtocolError> {
        let target_id = match &target.target_id {
            Some(target_id) => target_id,
            None => self
                .by_owner
                .get(&target.owner)
                .ok_or_else(|| ProtocolError::not_found("the owner holds no target"))?,
        };
        let found = self
            .by_id
            .get(target_id)
            .ok_or_else(|| ProtocolError::not_found(format!("unknown target: {target_id}")))?;
        if found.owner != target.owner {
            return Err(owner_mismatch());
        }
        Ok(found)
    }

    fn find_mut(&mut self, environment_id: &str) -> Option<&mut FakeTarget> {
        self.by_id
            .values_mut()
            .find(|target| target.owner.environment_id.as_str() == environment_id)
    }

    fn remove(&mut self, target_id: &ProviderTargetId) {
        if let Some(target) = self.by_id.remove(target_id) {
            self.by_owner.remove(&target.owner);
        }
    }

    fn create(
        &mut self,
        params: CreateTargetParams,
        booting_dials: u32,
    ) -> Result<TargetObservation, ProtocolError> {
        if let Some(idle) = &params.idle {
            idle.validate()
                .map_err(|error| ProtocolError::invalid_field("idle", error.to_string()))?;
        }
        if let Some(target_id) = self.by_owner.get(&params.owner) {
            return Ok(self.by_id[target_id].observation());
        }
        if params.template_id.as_str() != FAKE_TEMPLATE_ID {
            return Err(ProtocolError::not_found(format!(
                "unknown template: {}",
                params.template_id
            )));
        }
        let mut identity = DefaultHasher::new();
        params.owner.hash(&mut identity);
        let target_id = ProviderTargetId::new(format!("fake-{:016x}", identity.finish()))
            .expect("valid fake target id");
        let target = FakeTarget {
            target_id: target_id.clone(),
            owner: params.owner.clone(),
            power: PowerState::Running,
            ingress: false,
            booting_dials,
        };
        let observation = target.observation();
        self.by_owner.insert(params.owner, target_id.clone());
        self.by_id.insert(target_id, target);
        Ok(observation)
    }

    fn set_power(
        &mut self,
        params: SetTargetPowerParams,
    ) -> Result<TargetObservation, ProtocolError> {
        if !FAKE_POWER_STATES.contains(&params.power) {
            return Err(ProtocolError::invalid_field(
                "power",
                format!("unsupported power state {}", params.power),
            ));
        }
        let target_id = self.resolve(&params.target)?.target_id.clone();
        let target = self.by_id.get_mut(&target_id).expect("resolved target");
        target.power = params.power;
        Ok(target.observation())
    }

    fn close(&mut self, params: CloseTargetParams) -> Result<TargetObservation, ProtocolError> {
        let closed = |target_id| TargetObservation {
            target_id,
            owner: params.target.owner.clone(),
            phase: TargetPhase::Closed,
            power: None,
            message: None,
            ingress: None,
        };
        let target_id = match self.resolve(&params.target) {
            Ok(target) => target.target_id.clone(),
            Err(error) if error.kind() == ProtocolErrorKind::NotFound => {
                return Ok(closed(params.target.target_id.clone()));
            }
            Err(error) => return Err(error),
        };
        self.remove(&target_id);
        Ok(closed(Some(target_id)))
    }

    fn set_ingress(
        &mut self,
        target: &TargetRef,
        enabled: bool,
    ) -> Result<TargetObservation, ProtocolError> {
        let target_id = self.resolve(target)?.target_id.clone();
        let target = self.by_id.get_mut(&target_id).expect("resolved target");
        target.ingress = enabled;
        Ok(target.observation())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environments::gateway::runtime_client_info;
    use environment_client::{
        ConnectOptions, ControllerClient, DataClient,
        conformance::{
            controller::{ControllerConformance, run},
            data_route::expect_refusal,
        },
    };
    use environment_protocol::routes::{DataRouteBase, provider_data_route};
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    fn owner_factory() -> Arc<dyn Fn() -> TargetOwner + Send + Sync> {
        let next = Arc::new(AtomicU64::new(0));
        let universe = uuid::Uuid::new_v4().to_string();
        Arc::new(move || {
            let n = next.fetch_add(1, Ordering::SeqCst);
            TargetOwner {
                universe_id: UniverseId::new(universe.clone()).unwrap(),
                environment_id: EnvironmentId::new(format!("environment_{n}")).unwrap(),
            }
        })
    }

    fn options(controller: &FakeController) -> ConnectOptions {
        let options = ConnectOptions::new(runtime_client_info());
        match controller.credential() {
            Some(credential) => options.with_bearer_token(credential),
            None => options,
        }
    }

    async fn conformance(options_for: FakeControllerOptions) {
        let controller = FakeController::start_with(options_for).await.unwrap();
        let mut suite = ControllerConformance::new(
            controller.controller_url(),
            options(&controller),
            TemplateId::new(FAKE_TEMPLATE_ID).unwrap(),
            owner_factory(),
        );
        suite.prompt_bound = Duration::from_secs(5);
        suite.settle_timeout = Duration::from_secs(10);
        suite.poll_interval = Duration::from_millis(50);
        suite.data_route_base =
            Some(DataRouteBase::derived_from_controller(controller.controller_url()).unwrap());
        run(&suite)
            .await
            .into_result()
            .expect("the fake controller conforms");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_fake_controller_conforms() {
        conformance(FakeControllerOptions::default()).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_fake_controller_without_optional_capabilities_conforms() {
        conformance(FakeControllerOptions {
            power: false,
            ingress: false,
            ..FakeControllerOptions::default()
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_fake_controller_without_a_credential_conforms() {
        conformance(FakeControllerOptions {
            credential: false,
            ..FakeControllerOptions::default()
        })
        .await;
    }

    /// A target created through the controller, and its data route under
    /// the base Lightspeed derives from the controller URL.
    async fn created(controller: &FakeController) -> (TargetOwner, String) {
        let (client, _) =
            ControllerClient::connect_initialized(controller.controller_url(), options(controller))
                .await
                .expect("controller handshake");
        let owner = owner_factory()();
        let target_id = client
            .create_target(&CreateTargetParams {
                owner: owner.clone(),
                template_id: TemplateId::new(FAKE_TEMPLATE_ID).unwrap(),
                display_name: None,
                idle: None,
            })
            .await
            .expect("create")
            .target
            .target_id
            .expect("target id");
        let _ = client.close().await;
        let base = DataRouteBase::derived_from_controller(controller.controller_url()).unwrap();
        let route = provider_data_route(&base, &owner, &target_id);
        (owner, route)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_dial_wakes_a_powered_down_target_and_refuses_waking_once() {
        let controller = FakeController::start().await.unwrap();
        let (owner, route) = created(&controller).await;
        assert!(
            controller
                .set_power_out_of_band(owner.environment_id.as_str(), PowerState::Paused)
                .await
        );
        let refusal = expect_refusal(&route, options(&controller))
            .await
            .expect("a conforming refusal");
        assert_eq!(refusal, DataRouteRefusal::Waking {});
        let (client, _) = DataClient::connect_initialized(&route, options(&controller))
            .await
            .expect("the woken target upgrades");
        let _ = client.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_new_target_refuses_booting_for_its_configured_dials() {
        let controller = FakeController::start_with(FakeControllerOptions {
            booting_dials: 1,
            ..FakeControllerOptions::default()
        })
        .await
        .unwrap();
        let (_, route) = created(&controller).await;
        assert_eq!(
            expect_refusal(&route, options(&controller)).await.unwrap(),
            DataRouteRefusal::Booting {}
        );
        let (client, _) = DataClient::connect_initialized(&route, options(&controller))
            .await
            .expect("envd answers after its booting dials");
        let _ = client.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_ended_target_refuses_gone_and_an_unreachable_controller_refuses_all() {
        let controller = FakeController::start().await.unwrap();
        let (owner, route) = created(&controller).await;
        assert!(controller.end_target(owner.environment_id.as_str()).await);
        assert_eq!(controller.target_count().await, 0);
        assert_eq!(
            expect_refusal(&route, options(&controller)).await.unwrap(),
            DataRouteRefusal::Gone {}
        );
        controller.set_reachable(false);
        let error = ControllerClient::connect_initialized(
            controller.controller_url(),
            options(&controller),
        )
        .await
        .expect_err("an unreachable controller refuses the upgrade");
        assert!(matches!(
            error,
            environment_client::ClientError::UpgradeRejected { status: 503, .. }
        ));
    }
}
