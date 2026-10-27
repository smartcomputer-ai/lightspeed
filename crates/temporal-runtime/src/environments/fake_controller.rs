//! An in-memory provider controller for tests.
//!
//! It serves the controller plane on `/control` and a minimal data plane on
//! `/routes/{universeId}/{bindingId}/{environmentId}/{incarnationId}/{targetId}`
//! over a loopback WebSocket listener, and checks the provider credential
//! as a bearer token on both, exactly as a real provider must. Nothing
//! registers it: a test starts one and registers its controller URL and
//! credential like any provider's.
//!
//! Targets come up at once: `target/create` answers an `active`, `running`
//! target whose daemon is ready, and power changes and closes complete
//! before they answer. The data route answers `initialize` only, as an envd
//! with no capability families, which is enough for the runtime's
//! data-plane handshake at the Ready transition.

use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
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
            ControllerRoutes,
        },
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            AdoptTargetParams, CloseTargetParams, CreateTargetParams, GetTargetParams,
            IngressObservation, PowerState, SetTargetPowerParams, TargetObservation, TargetOwner,
            TargetPhase, TargetRef, TargetResponse, TargetState,
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
    ids::{
        BindingId, DaemonInstanceId, EnvironmentId, IncarnationId, ProviderTargetId, TemplateId,
        UniverseId,
    },
    jsonrpc::{self, Request, Response as RpcResponse},
    limits::{MAX_MESSAGE_BYTES, MIN_CONCURRENT_REQUESTS},
    manifest::{
        ControllerInitialize, IngressEnsure, IngressRemove, RequestMethod, TargetAdopt,
        TargetClose, TargetCreate, TargetGet, TargetSetPower, TemplateList,
    },
    metadata::{DescriptiveText, DisplayName, Metadata},
    paths::EnvironmentPath,
    routes::{DataRouteBase, PublicUrl},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

/// The one template the fake controller offers.
pub const FAKE_TEMPLATE_ID: &str = "rust-v1";

/// Public endpoint the fake controller reports for enabled ingress.
pub const FAKE_INGRESS_ENDPOINT: &str = "https://fake.env.test";

/// Power states the fake controller supports. Suspended is included so the
/// suspend path has coverage.
pub const FAKE_POWER_STATES: [PowerState; 4] = [
    PowerState::Running,
    PowerState::Paused,
    PowerState::Suspended,
    PowerState::Stopped,
];

/// What the fake controller advertises.
#[derive(Clone, Debug)]
pub struct FakeControllerOptions {
    /// `capabilities.adoptTarget`.
    pub adopt_target: bool,
    /// `capabilities.ingress`.
    pub ingress: bool,
    /// The protocol version the fake envd behind every data route speaks.
    pub daemon_protocol_version: u32,
}

impl Default for FakeControllerOptions {
    fn default() -> Self {
        Self {
            adopt_target: true,
            ingress: true,
            daemon_protocol_version: ENVD_PROTOCOL_VERSION,
        }
    }
}

/// A running fake controller. Dropping it stops the listener.
pub struct FakeController {
    controller_url: String,
    credential: String,
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
        let credential = format!("fake-credential-{}", uuid::Uuid::new_v4().simple());
        let data = DataRouteBase::new(format!("ws://{address}/routes"))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let initialize = ControllerInitializeResponse {
            protocol_version: CONTROLLER_PROTOCOL_VERSION,
            plane: Plane::Controller,
            server_info: implementation("lightspeed-fake-controller"),
            capabilities: ControllerCapabilities {
                adopt_target: options.adopt_target,
                ingress: options.ingress,
            },
            routes: ControllerRoutes { data },
        };
        let app = Arc::new(App {
            credential: credential.clone(),
            options,
            initialize,
            targets: Mutex::new(Targets::default()),
        });
        let router = Router::new()
            .route("/control", get(control_upgrade))
            .route(
                "/routes/{universe}/{binding}/{environment}/{incarnation}/{target}",
                get(data_upgrade),
            )
            .with_state(app);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            controller_url: format!("ws://{address}/control"),
            credential,
            task,
        })
    }

    /// The controller URL to register the provider with.
    pub fn controller_url(&self) -> &str {
        &self.controller_url
    }

    /// The provider credential the controller and its data routes accept.
    pub fn credential(&self) -> &str {
        &self.credential
    }
}

impl Drop for FakeController {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct App {
    credential: String,
    options: FakeControllerOptions,
    initialize: ControllerInitializeResponse,
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
    metadata: Metadata,
}

impl FakeTarget {
    fn observation(&self) -> TargetObservation {
        TargetObservation {
            target_id: Some(self.target_id.clone()),
            owner: self.owner.clone(),
            display_name: DisplayName::new(self.owner.environment_id.as_str()).ok(),
            state: TargetState {
                phase: TargetPhase::Active,
                power: Some(self.power),
                daemon_ready: self.power == PowerState::Running,
                message: None,
            },
            supported_power_states: FAKE_POWER_STATES.to_vec(),
            ingress: self.ingress.then(|| IngressObservation {
                endpoint: PublicUrl::new(FAKE_INGRESS_ENDPOINT).expect("valid fake endpoint"),
            }),
            metadata: self.metadata.clone(),
        }
    }
}

fn implementation(name: &str) -> ImplementationInfo {
    ImplementationInfo::new(name)
        .and_then(|info| info.with_version(env!("CARGO_PKG_VERSION")))
        .expect("fake implementation info is within bounds")
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), app.credential.as_bytes()))
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

async fn control_upgrade(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorized(&app, &headers) {
        return unauthorized();
    }
    with_limits(upgrade)
        .on_upgrade(move |socket| serve(socket, Plane::Controller, app))
        .into_response()
}

async fn data_upgrade(
    State(app): State<Arc<App>>,
    Path((universe, binding, environment, incarnation, target_id)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorized(&app, &headers) {
        return unauthorized();
    }
    let (Ok(universe_id), Ok(binding_id), Ok(environment_id), Ok(incarnation_id), Ok(target_id)) = (
        UniverseId::new(universe),
        BindingId::new(binding),
        EnvironmentId::new(environment),
        IncarnationId::new(incarnation),
        ProviderTargetId::new(target_id),
    ) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let owner = TargetOwner {
        universe_id,
        binding_id,
        environment_id,
        incarnation_id,
    };
    match app.targets.lock().await.by_id.get(&target_id) {
        Some(target) if target.owner != owner => return StatusCode::NOT_FOUND.into_response(),
        Some(target) if target.power != PowerState::Running => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Some(_) => {}
        None => return StatusCode::NOT_FOUND.into_response(),
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
        return encode(&list_templates());
    }
    if (method == IngressEnsure::NAME || method == IngressRemove::NAME) && !app.options.ingress {
        return Err(ProtocolError::capability_unavailable(
            Capability::Ingress,
            "the fake controller was started without ingress",
        ));
    }
    if method == TargetAdopt::NAME && !app.options.adopt_target {
        return Err(ProtocolError::capability_unavailable(
            Capability::AdoptTarget,
            "the fake controller was started without adoption",
        ));
    }
    let mut targets = app.targets.lock().await;
    let observation = match method {
        TargetCreate::NAME => targets.create(params::<TargetCreate>(request)?)?,
        TargetAdopt::NAME => targets.adopt(params::<TargetAdopt>(request)?)?,
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

fn list_templates() -> ListTemplatesResponse {
    ListTemplatesResponse {
        templates: vec![EnvironmentTemplate {
            template_id: TemplateId::new(FAKE_TEMPLATE_ID).expect("valid template id"),
            display_name: DisplayName::new("Rust development").expect("valid display name"),
            description: DescriptiveText::new("Deterministic fake provider template").ok(),
            public_ingress: true,
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

    fn insert(&mut self, owner: TargetOwner, metadata: Metadata) -> TargetObservation {
        let mut identity = DefaultHasher::new();
        owner.hash(&mut identity);
        let target_id = ProviderTargetId::new(format!("fake-{:016x}", identity.finish()))
            .expect("valid fake target id");
        let target = FakeTarget {
            target_id: target_id.clone(),
            owner: owner.clone(),
            power: PowerState::Running,
            ingress: false,
            metadata,
        };
        let observation = target.observation();
        self.by_owner.insert(owner, target_id.clone());
        self.by_id.insert(target_id, target);
        observation
    }

    fn create(&mut self, params: CreateTargetParams) -> Result<TargetObservation, ProtocolError> {
        if let Some(target_id) = self.by_owner.get(&params.owner) {
            return Ok(self.by_id[target_id].observation());
        }
        if params.template_id.as_str() != FAKE_TEMPLATE_ID {
            return Err(ProtocolError::not_found(format!(
                "unknown template: {}",
                params.template_id
            )));
        }
        let metadata = Metadata::new(BTreeMap::from([(
            "imageFingerprint".to_owned(),
            format!("fake:{}", params.template_id),
        )]))
        .expect("valid fake metadata");
        Ok(self.insert(params.owner, metadata))
    }

    fn adopt(&mut self, params: AdoptTargetParams) -> Result<TargetObservation, ProtocolError> {
        let fingerprint = format!("fake:adopted:{}", params.source_target);
        if let Some(target_id) = self.by_owner.get(&params.owner) {
            let target = &self.by_id[target_id];
            return if target.metadata.get("imageFingerprint") == Some(fingerprint.as_str()) {
                Ok(target.observation())
            } else {
                Err(owner_mismatch())
            };
        }
        if self
            .by_id
            .values()
            .any(|target| target.metadata.get("imageFingerprint") == Some(fingerprint.as_str()))
        {
            return Err(owner_mismatch());
        }
        let metadata = Metadata::new(BTreeMap::from([(
            "imageFingerprint".to_owned(),
            fingerprint,
        )]))
        .map_err(|error| ProtocolError::invalid_params(error.to_string()))?;
        Ok(self.insert(params.owner, metadata))
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
        };
        let target_id = match self.resolve(&params.target) {
            Ok(target) => target.target_id.clone(),
            Err(error) if error.kind() == ProtocolErrorKind::NotFound => {
                return Ok(closed(None));
            }
            Err(error) => return Err(error),
        };
        if let Some(target) = self.by_id.remove(&target_id) {
            self.by_owner.remove(&target.owner);
        }
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
        ConnectOptions,
        conformance::controller::{ControllerConformance, run},
    };
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
                binding_id: BindingId::new("conformance").unwrap(),
                environment_id: EnvironmentId::new(format!("environment_{n}")).unwrap(),
                incarnation_id: IncarnationId::new(format!("incarnation_{n}")).unwrap(),
            }
        })
    }

    async fn conformance(options: FakeControllerOptions) {
        let controller = FakeController::start_with(options).await.unwrap();
        let mut suite = ControllerConformance::new(
            controller.controller_url(),
            ConnectOptions::new(runtime_client_info()).with_bearer_token(controller.credential()),
            TemplateId::new(FAKE_TEMPLATE_ID).unwrap(),
            owner_factory(),
        );
        suite.prompt_bound = Duration::from_secs(5);
        suite.settle_timeout = Duration::from_secs(10);
        suite.poll_interval = Duration::from_millis(50);
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
    async fn the_fake_controller_without_optional_features_conforms() {
        conformance(FakeControllerOptions {
            adopt_target: false,
            ingress: false,
            ..FakeControllerOptions::default()
        })
        .await;
    }
}
