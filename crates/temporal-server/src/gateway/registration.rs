//! Outbound `envd` registration on the environment gateway.
//!
//! One public route accepts control connections: the daemon proves its
//! identity against a nonce, is admitted by a registration key the first
//! time and by its identity afterwards, and then holds the socket while the
//! gateway pings it and asks it to open data connections. A second public
//! route accepts those reverse-dialed data sockets and pairs each with the
//! worker route that requested it; the pair is then proxied frame for frame
//! exactly like a passive external environment.
//!
//! The gateway holds no durable lifecycle authority here. Connect,
//! heartbeat, and disconnect are written as observations on the environment
//! row; the lifecycle reconciler derives ephemeral cleanup and stale
//! repair from them.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use environment_protocol::{
    registration::{
        DATA_PATH, DaemonControlMessage, GatewayControlMessage, REGISTRATION_NONCE_BYTES,
        RegisterParams, RegistrationReceipt, RegistrationRejectionCode, decode_hex, encode_hex,
        signed_registration_message, validate_registration_metadata,
    },
    shared::CURRENT_PROTOCOL_VERSION,
};
use environments::{
    CreateRegisteredEnvironment, EnvironmentRecord, EnvironmentRegistrationKeyStore,
    EnvironmentRegistryError, EnvironmentStatus, EnvironmentStore, ObserveRegisteredEnvironment,
    RegisteredConnectionObservation, registration_key_hash, validate_daemon_public_key,
};
use tokio::sync::{Semaphore, mpsc, oneshot};
use uuid::Uuid;

use super::http::GatewayState;
use crate::environments::gateway::{RouteKey, close_message};

/// Interval between gateway pings on a control connection; each pong
/// refreshes the environment's heartbeat stamp.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Time a daemon has to answer the challenge.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Time a daemon has to dial the data route for one open request.
const DATA_DIAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Interval at which a control connection re-reads durable state.
const REAUTHORIZE_INTERVAL: Duration = Duration::from_secs(5);
/// Unauthenticated handshakes in flight at once.
const MAX_CONCURRENT_HANDSHAKES: usize = 256;
/// Failed authentications per key prefix (or daemon) per window.
const MAX_FAILURES_PER_WINDOW: usize = 20;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// Control frames are small JSON documents; anything larger is hostile.
const CONTROL_MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Live state of registered daemons on this gateway replica. In-memory by
/// design: durable truth is the environment row.
pub struct RegisteredConnections {
    controls: Mutex<HashMap<(Uuid, String), ControlHandle>>,
    pending: Mutex<HashMap<String, PendingData>>,
    handshakes: Arc<Semaphore>,
    failures: Mutex<HashMap<String, Vec<Instant>>>,
}

struct ControlHandle {
    connection_id: String,
    incarnation_id: String,
    commands: mpsc::Sender<ControlCommand>,
}

enum ControlCommand {
    OpenData { token: String, data_url: String },
    Supersede,
}

struct PendingData {
    sender: oneshot::Sender<WebSocket>,
    expires_at: Instant,
}

impl Default for RegisteredConnections {
    fn default() -> Self {
        Self::new()
    }
}

impl RegisteredConnections {
    pub fn new() -> Self {
        Self {
            controls: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            handshakes: Arc::new(Semaphore::new(MAX_CONCURRENT_HANDSHAKES)),
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// The control connection currently serving an environment, if any.
    pub fn current_connection_id(&self, key: &RouteKey) -> Option<String> {
        self.controls
            .lock()
            .expect("registered connections poisoned")
            .get(&(key.universe_id, key.environment_id.clone()))
            .map(|handle| handle.connection_id.clone())
    }

    fn install(
        &self,
        universe_id: Uuid,
        environment_id: String,
        handle: ControlHandle,
    ) -> Option<ControlHandle> {
        self.controls
            .lock()
            .expect("registered connections poisoned")
            .insert((universe_id, environment_id), handle)
    }

    /// Remove the handle only if it is still the current one; returns true
    /// when this connection was current until now.
    fn remove_if_current(
        &self,
        universe_id: Uuid,
        environment_id: &str,
        connection_id: &str,
    ) -> bool {
        let mut controls = self
            .controls
            .lock()
            .expect("registered connections poisoned");
        let key = (universe_id, environment_id.to_owned());
        if controls
            .get(&key)
            .is_some_and(|handle| handle.connection_id == connection_id)
        {
            controls.remove(&key);
            true
        } else {
            false
        }
    }

    fn record_failure(&self, subject: &str) -> bool {
        let now = Instant::now();
        let mut failures = self.failures.lock().expect("failures poisoned");
        let entry = failures.entry(subject.to_owned()).or_default();
        entry.retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
        entry.push(now);
        entry.len() > MAX_FAILURES_PER_WINDOW
    }

    fn over_failure_limit(&self, subject: &str) -> bool {
        let now = Instant::now();
        let failures = self.failures.lock().expect("failures poisoned");
        failures.get(subject).is_some_and(|entries| {
            entries
                .iter()
                .filter(|at| now.duration_since(**at) < FAILURE_WINDOW)
                .count()
                >= MAX_FAILURES_PER_WINDOW
        })
    }

    fn take_pending(&self, token: &str) -> Option<PendingData> {
        let mut pending = self.pending.lock().expect("pending data poisoned");
        let now = Instant::now();
        pending.retain(|_, entry| entry.expires_at > now);
        pending.remove(token)
    }
}

/// Outcome of admitting one register request against durable state.
#[derive(Debug)]
pub struct Admission {
    pub environment: EnvironmentRecord,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRejection {
    pub code: RegistrationRejectionCode,
    pub message: String,
}

impl RegistrationRejection {
    fn new(code: RegistrationRejectionCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Check a register request's shape and signature against the challenge
/// nonce. Pure: no store access.
pub fn verify_register_request(
    params: &RegisterParams,
    nonce: &[u8],
) -> Result<(), RegistrationRejection> {
    if params.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(RegistrationRejection::new(
            RegistrationRejectionCode::UnsupportedProtocol,
            format!(
                "unsupported registration protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                params.protocol_version
            ),
        ));
    }
    validate_daemon_public_key(&params.daemon_public_key).map_err(|error| {
        RegistrationRejection::new(RegistrationRejectionCode::InvalidRequest, error.to_string())
    })?;
    validate_registration_metadata(params.display_name.as_deref(), &params.metadata).map_err(
        |message| RegistrationRejection::new(RegistrationRejectionCode::InvalidRequest, message),
    )?;
    let public_key = decode_hex(&params.daemon_public_key)
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok())
        .ok_or_else(|| {
            RegistrationRejection::new(
                RegistrationRejectionCode::InvalidSignature,
                "daemon public key is not a valid Ed25519 key",
            )
        })?;
    let signature = decode_hex(&params.signature)
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or_else(|| {
            RegistrationRejection::new(
                RegistrationRejectionCode::InvalidSignature,
                "signature is not a valid Ed25519 signature",
            )
        })?;
    public_key
        .verify(&signed_registration_message(nonce), &signature)
        .map_err(|_| {
            RegistrationRejection::new(
                RegistrationRejectionCode::InvalidSignature,
                "signature does not verify against the challenge",
            )
        })
}

/// Access anchors of the environments registration creates. The principal
/// that minted the admitting key owns them.
#[async_trait::async_trait]
pub trait RegistrationAnchors: Send + Sync {
    /// Reserve the anchor of a new environment before its row exists.
    async fn reserve(
        &self,
        registration_key_id: &environments::EnvironmentRegistrationKeyId,
        environment_id: &environments::EnvironmentId,
    ) -> Result<(), RegistrationRejection>;
    /// Release an anchor under which no environment was created.
    async fn release(&self, environment_id: &environments::EnvironmentId);
}

/// Anchors in a universe's access store.
pub(crate) struct PgRegistrationAnchors {
    pub store: Arc<store_pg::PgStore>,
    pub universe_id: Uuid,
}

#[async_trait::async_trait]
impl RegistrationAnchors for PgRegistrationAnchors {
    async fn reserve(
        &self,
        registration_key_id: &environments::EnvironmentRegistrationKeyId,
        environment_id: &environments::EnvironmentId,
    ) -> Result<(), RegistrationRejection> {
        let owner = self
            .store
            .registration_key_creator(registration_key_id)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| {
                RegistrationRejection::new(
                    RegistrationRejectionCode::InvalidRegistrationKey,
                    "registration key records no creator to own its environments; issue a new key",
                )
            })?;
        store_pg::PgAccessStore::new(self.store.pool().clone())
            .reserve_resource(
                self.universe_id,
                &access::ResourceRef::Environment(environment_id.to_string()),
                &access::ActionActor::Internal {
                    component: "registration".into(),
                    cause: registration_key_id.to_string(),
                },
                &access::ResourceController::Principal(owner),
                None,
                None,
                u64::try_from(now_ms()).unwrap_or_default(),
            )
            .await
            .map(|_| ())
            .map_err(|error| {
                tracing::warn!(target: "temporal_server", %error, "registration anchor failed");
                RegistrationRejection::new(
                    RegistrationRejectionCode::Unavailable,
                    "registration is temporarily unavailable",
                )
            })
    }

    async fn release(&self, environment_id: &environments::EnvironmentId) {
        let released = store_pg::PgAccessStore::new(self.store.pool().clone())
            .release_resource(
                self.universe_id,
                &access::ResourceRef::Environment(environment_id.to_string()),
            )
            .await;
        if let Err(error) = released {
            tracing::warn!(target: "temporal_server", %error, "releasing a registration anchor failed");
        }
    }
}

/// Admit a verified register request inside its universe: reconnect a known
/// daemon identity, or create an environment for a new one through its
/// registration key. Every refusal is typed and leaves no rows behind.
pub async fn admit_registration<S>(
    store: &S,
    anchors: &dyn RegistrationAnchors,
    params: &RegisterParams,
    now_ms: i64,
) -> Result<Admission, RegistrationRejection>
where
    S: EnvironmentStore + EnvironmentRegistrationKeyStore + ?Sized,
{
    let known = store
        .read_environment_by_daemon_public_key(&params.daemon_public_key)
        .await
        .map_err(unavailable)?;
    if let Some(environment) = known {
        if matches!(
            environment.status,
            EnvironmentStatus::Closing | EnvironmentStatus::Closed
        ) {
            return Err(RegistrationRejection::new(
                RegistrationRejectionCode::EnvironmentClosed,
                format!(
                    "environment {} is closed; this daemon identity is spent",
                    environment.environment_id
                ),
            ));
        }
        return Ok(Admission {
            environment,
            created: false,
        });
    }
    let Some(secret) = params.registration_key.as_ref() else {
        return Err(RegistrationRejection::new(
            RegistrationRejectionCode::UnknownDaemon,
            "unknown daemon identity and no registration key presented",
        ));
    };
    let key = store
        .resolve_registration_key(&registration_key_hash(secret.expose()))
        .await
        .map_err(unavailable)?
        .ok_or_else(|| {
            RegistrationRejection::new(
                RegistrationRejectionCode::InvalidRegistrationKey,
                "registration key is not recognized",
            )
        })?;
    key.check_admits(now_ms).map_err(map_registry_error)?;
    let mut metadata = params.metadata.clone();
    metadata.extend(
        environments::RegisteredDaemonBuild::from_registration(
            &params.implementation,
            params.protocol_version,
        )
        .metadata(),
    );
    let environment_id = super::service::allocate_environment_id_public();
    anchors
        .reserve(&key.registration_key_id, &environment_id)
        .await?;
    let created = store
        .create_registered_environment(CreateRegisteredEnvironment {
            registration_key_id: key.registration_key_id.clone(),
            environment_id: environment_id.clone(),
            incarnation_id: super::service::allocate_incarnation_id_public(),
            daemon_public_key: params.daemon_public_key.clone(),
            display_name: params.display_name.clone(),
            metadata,
            created_at_ms: now_ms,
        })
        .await;
    // A concurrent registration of the same daemon resolves to the
    // environment it created, under that one's own anchor.
    if !created
        .as_ref()
        .is_ok_and(|environment| environment.environment_id == environment_id)
    {
        anchors.release(&environment_id).await;
    }
    Ok(Admission {
        environment: created.map_err(map_registry_error)?,
        created: true,
    })
}

fn map_registry_error(error: EnvironmentRegistryError) -> RegistrationRejection {
    match error {
        EnvironmentRegistryError::RegistrationKeyUnavailable { reason, .. } => {
            RegistrationRejection::new(
                if reason == "expired" {
                    RegistrationRejectionCode::RegistrationKeyExpired
                } else {
                    RegistrationRejectionCode::RegistrationKeyRevoked
                },
                format!("registration key is {reason}"),
            )
        }
        EnvironmentRegistryError::RegistrationCapacityExhausted { limit, .. } => {
            RegistrationRejection::new(
                RegistrationRejectionCode::CapacityExhausted,
                format!("registration key has reached its active environment limit of {limit}"),
            )
        }
        EnvironmentRegistryError::AlreadyExists {
            kind: "daemon_identity",
            ..
        } => RegistrationRejection::new(
            RegistrationRejectionCode::IdentityInUse,
            "daemon identity is already bound to an environment elsewhere",
        ),
        EnvironmentRegistryError::InvalidInput { message } => {
            RegistrationRejection::new(RegistrationRejectionCode::InvalidRequest, message)
        }
        other => unavailable(other),
    }
}

fn unavailable(error: EnvironmentRegistryError) -> RegistrationRejection {
    tracing::warn!(target: "temporal_server", %error, "registration store failure");
    RegistrationRejection::new(
        RegistrationRejectionCode::Unavailable,
        "registration is temporarily unavailable",
    )
}

/// `GET /environment-gateway/connect`: the daemon's control connection.
pub(super) async fn connect_upgrade(
    State(state): State<Arc<GatewayState>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Ok(permit) = state.registrations().handshakes.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    upgrade
        .max_message_size(CONTROL_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let outcome = control_session(state, socket, permit).await;
            if let Err(error) = outcome {
                tracing::debug!(target: "temporal_server", %error, "registration control session ended with error");
            }
        })
        .into_response()
}

/// `GET /environment-gateway/data`: a reverse-dialed data socket presenting
/// its one-time token as a bearer header.
pub(super) async fn data_upgrade(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(pending) = state.registrations().take_pending(&token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .max_message_size(64 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            // The waiting route owns the socket from here; if it gave up,
            // dropping the socket closes the daemon's dial.
            let _ = pending.sender.send(socket);
        })
        .into_response()
}

/// Ask the daemon serving `key` to dial a data socket for one worker route
/// and wait for it. Returns the paired socket and the control connection it
/// was issued under, so the proxy can fence on it.
pub(super) async fn open_registered_route(
    state: &Arc<GatewayState>,
    key: &RouteKey,
) -> Result<(WebSocket, String), StatusCode> {
    let registrations = state.registrations();
    let (commands, control_connection_id) = {
        let controls = registrations
            .controls
            .lock()
            .expect("registered connections poisoned");
        let Some(handle) = controls.get(&(key.universe_id, key.environment_id.clone())) else {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        };
        if handle.incarnation_id != key.incarnation_id {
            return Err(StatusCode::CONFLICT);
        }
        (handle.commands.clone(), handle.connection_id.clone())
    };
    let token = new_token();
    let (sender, receiver) = oneshot::channel();
    registrations
        .pending
        .lock()
        .expect("pending data poisoned")
        .insert(
            token.clone(),
            PendingData {
                sender,
                expires_at: Instant::now() + DATA_DIAL_TIMEOUT,
            },
        );
    let sent = commands
        .send(ControlCommand::OpenData {
            token: token.clone(),
            data_url: state.registration_data_url(),
        })
        .await;
    if sent.is_err() {
        registrations.take_pending(&token);
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    match tokio::time::timeout(DATA_DIAL_TIMEOUT, receiver).await {
        Ok(Ok(socket)) => Ok((socket, control_connection_id)),
        _ => {
            registrations.take_pending(&token);
            Err(StatusCode::GATEWAY_TIMEOUT)
        }
    }
}

/// Pipe one worker route and one reverse-dialed daemon socket, re-checking
/// durable state and the current control connection on an interval.
pub(super) async fn proxy_registered_route(
    state: Arc<GatewayState>,
    key: RouteKey,
    control_connection_id: String,
    mut worker: WebSocket,
    mut daemon: WebSocket,
) {
    let mut reauthorize = tokio::time::interval(REAUTHORIZE_INTERVAL);
    reauthorize.tick().await;
    loop {
        tokio::select! {
            _ = reauthorize.tick() => {
                if authorize_registered_route(&state, &key, &control_connection_id).await.is_err() {
                    let _ = worker.send(close_message("registered route authorization changed")).await;
                    break;
                }
            }
            message = worker.recv() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, Message::Close(_));
                if daemon.send(message).await.is_err() || close { break }
            }
            message = daemon.recv() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, Message::Close(_));
                if worker.send(message).await.is_err() || close { break }
            }
        }
    }
    let _ = daemon.send(Message::Close(None)).await;
    let _ = worker.send(Message::Close(None)).await;
}

async fn authorize_registered_route(
    state: &GatewayState,
    key: &RouteKey,
    control_connection_id: &str,
) -> anyhow::Result<()> {
    if state.registrations().current_connection_id(key).as_deref() != Some(control_connection_id) {
        anyhow::bail!("control connection superseded");
    }
    let api = state
        .api_for_daemon(key.universe_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let environment_id = environments::EnvironmentId::try_new(key.environment_id.clone())?;
    let environment =
        EnvironmentStore::read_environment(api.store().as_ref(), &environment_id).await?;
    if !environment.is_registered()
        || environment.incarnation.incarnation_id.as_str() != key.incarnation_id
        || matches!(
            environment.status,
            EnvironmentStatus::Closing | EnvironmentStatus::Closed
        )
    {
        anyhow::bail!("registered route no longer valid");
    }
    Ok(())
}

async fn control_session(
    state: Arc<GatewayState>,
    mut socket: WebSocket,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let nonce = {
        use rand::RngCore as _;
        let mut bytes = [0u8; REGISTRATION_NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        bytes
    };
    send_control(
        &mut socket,
        &GatewayControlMessage::Challenge {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            nonce: encode_hex(&nonce),
        },
    )
    .await?;
    let register =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, next_daemon_message(&mut socket)).await {
            Ok(Ok(Some(DaemonControlMessage::Register(params)))) => params,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(error)) => {
                reject(
                    &mut socket,
                    RegistrationRejectionCode::InvalidRequest,
                    error.to_string(),
                )
                .await;
                return Ok(());
            }
            Err(_) => {
                reject(
                    &mut socket,
                    RegistrationRejectionCode::InvalidRequest,
                    "handshake timed out",
                )
                .await;
                return Ok(());
            }
        };
    let registrations = state.registrations();
    let subject = register
        .registration_key
        .as_ref()
        .map(|key| format!("key:{}", key.expose().chars().take(12).collect::<String>()))
        .unwrap_or_else(|| {
            format!(
                "daemon:{}",
                &register.daemon_public_key[..16.min(register.daemon_public_key.len())]
            )
        });
    if registrations.over_failure_limit(&subject) {
        reject(
            &mut socket,
            RegistrationRejectionCode::RateLimited,
            "too many failed registrations; retry later",
        )
        .await;
        return Ok(());
    }
    if let Err(rejection) = verify_register_request(&register, &nonce) {
        registrations.record_failure(&subject);
        reject(&mut socket, rejection.code, rejection.message).await;
        return Ok(());
    }
    let universe_id = match state.registration_universe(&register).await {
        Ok(Some(universe_id)) => universe_id,
        Ok(None) => {
            registrations.record_failure(&subject);
            let (code, message) = if register.registration_key.is_some() {
                (
                    RegistrationRejectionCode::InvalidRegistrationKey,
                    "registration key is not recognized",
                )
            } else {
                (
                    RegistrationRejectionCode::UnknownDaemon,
                    "unknown daemon identity and no registration key presented",
                )
            };
            reject(&mut socket, code, message).await;
            return Ok(());
        }
        Err(error) => {
            tracing::warn!(target: "temporal_server", error = %error.message, "registration universe lookup failed");
            reject(
                &mut socket,
                RegistrationRejectionCode::Unavailable,
                "registration is temporarily unavailable",
            )
            .await;
            return Ok(());
        }
    };
    let api = match state.api_for_daemon(universe_id).await {
        Ok(api) => api,
        Err(error) => {
            tracing::warn!(target: "temporal_server", error = %error.message, "registration universe unavailable");
            reject(
                &mut socket,
                RegistrationRejectionCode::Unavailable,
                "registration is temporarily unavailable",
            )
            .await;
            return Ok(());
        }
    };
    let store = api.store().clone();
    let anchors = PgRegistrationAnchors {
        store: store.clone(),
        universe_id,
    };
    let admission = match admit_registration(store.as_ref(), &anchors, &register, now_ms()).await {
        Ok(admission) => admission,
        Err(rejection) => {
            if matches!(
                rejection.code,
                RegistrationRejectionCode::InvalidRegistrationKey
                    | RegistrationRejectionCode::RegistrationKeyRevoked
                    | RegistrationRejectionCode::RegistrationKeyExpired
            ) {
                registrations.record_failure(&subject);
            }
            reject(&mut socket, rejection.code, rejection.message).await;
            return Ok(());
        }
    };
    let environment = admission.environment;
    let environment_id = environment.environment_id.to_string();
    let connection_id = format!("connection_{}", Uuid::new_v4().simple());
    let (commands, mut command_rx) = mpsc::channel::<ControlCommand>(16);
    if let Some(previous) = registrations.install(
        universe_id,
        environment_id.clone(),
        ControlHandle {
            connection_id: connection_id.clone(),
            incarnation_id: environment.incarnation.incarnation_id.to_string(),
            commands,
        },
    ) {
        let _ = previous.commands.try_send(ControlCommand::Supersede);
    }
    // Every admission re-records which build serves the environment, so a
    // replaced daemon shows its new version, commit, and protocol on the row
    // instead of the one it first registered with.
    let daemon_build = environments::RegisteredDaemonBuild::from_registration(
        &register.implementation,
        register.protocol_version,
    );
    let observe = |observation: RegisteredConnectionObservation| {
        let store = store.clone();
        let environment_id = environment.environment_id.clone();
        let metadata = match observation {
            RegisteredConnectionObservation::Connected => daemon_build.metadata(),
            _ => std::collections::BTreeMap::new(),
        };
        async move {
            let result = EnvironmentStore::observe_registered_environment(
                store.as_ref(),
                ObserveRegisteredEnvironment {
                    environment_id,
                    observation,
                    observed_at_ms: now_ms(),
                    metadata,
                },
            )
            .await;
            if let Err(error) = result {
                tracing::warn!(target: "temporal_server", %error, "registered observation failed");
            }
        }
    };
    observe(RegisteredConnectionObservation::Connected).await;
    let identity_mode = environment
        .source
        .identity_mode()
        .map(|mode| mode.as_str().to_owned())
        .unwrap_or_default();
    let daemon_id = match &environment.source {
        environments::EnvironmentSource::Registered { daemon_id, .. } => daemon_id.to_string(),
        _ => String::new(),
    };
    tracing::info!(
        target: "temporal_server",
        %universe_id,
        environment = %environment_id,
        daemon = %daemon_id,
        connection = %connection_id,
        created = admission.created,
        "registered environment control connection admitted"
    );
    send_control(
        &mut socket,
        &GatewayControlMessage::Accepted {
            receipt: RegistrationReceipt {
                environment_id: environment_id.clone(),
                incarnation_id: environment.incarnation.incarnation_id.to_string(),
                daemon_id,
                connection_id: connection_id.clone(),
                identity_mode,
            },
            heartbeat_interval_ms: HEARTBEAT_INTERVAL.as_millis() as u64,
        },
    )
    .await?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    let mut reauthorize = tokio::time::interval(REAUTHORIZE_INTERVAL);
    reauthorize.tick().await;
    let end_reason: &str = loop {
        tokio::select! {
            command = command_rx.recv() => match command {
                Some(ControlCommand::OpenData { token, data_url }) => {
                    let message = GatewayControlMessage::OpenData {
                        token: token.into(),
                        data_url,
                    };
                    if send_control(&mut socket, &message).await.is_err() {
                        break "send failed";
                    }
                }
                Some(ControlCommand::Supersede) => {
                    let _ = socket.send(close_message("superseded by a newer connection of the same daemon")).await;
                    break "superseded";
                }
                None => break "command channel closed",
            },
            _ = heartbeat.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break "ping failed";
                }
            }
            _ = reauthorize.tick() => {
                match EnvironmentStore::read_environment(store.as_ref(), &environment.environment_id).await {
                    Ok(current) if matches!(current.status, EnvironmentStatus::Closing | EnvironmentStatus::Closed) => {
                        let _ = socket.send(close_message("environment closed")).await;
                        break "environment closed";
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(target: "temporal_server", %error, "registered control reauthorization failed");
                    }
                }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Pong(_))) => observe(RegisteredConnectionObservation::Heartbeat).await,
                Some(Ok(Message::Ping(bytes))) => {
                    if socket.send(Message::Pong(bytes)).await.is_err() {
                        break "pong failed";
                    }
                }
                Some(Ok(Message::Close(_))) | None => break "daemon closed",
                Some(Ok(_)) => {}
                Some(Err(_)) => break "socket error",
            },
        }
    };
    let was_current = registrations.remove_if_current(universe_id, &environment_id, &connection_id);
    tracing::info!(
        target: "temporal_server",
        %universe_id,
        environment = %environment_id,
        connection = %connection_id,
        reason = end_reason,
        current = was_current,
        "registered environment control connection ended"
    );
    if was_current {
        observe(RegisteredConnectionObservation::Disconnected).await;
    }
    Ok(())
}

async fn send_control(
    socket: &mut WebSocket,
    message: &GatewayControlMessage,
) -> anyhow::Result<()> {
    socket
        .send(Message::Text(serde_json::to_string(message)?))
        .await
        .map_err(|error| anyhow::anyhow!("send control message: {error}"))
}

async fn reject(
    socket: &mut WebSocket,
    code: RegistrationRejectionCode,
    message: impl Into<String>,
) {
    let message = message.into();
    tracing::info!(target: "temporal_server", ?code, %message, "registration rejected");
    let _ = send_control(socket, &GatewayControlMessage::Rejected { code, message }).await;
    let _ = socket.send(Message::Close(None)).await;
}

async fn next_daemon_message(
    socket: &mut WebSocket,
) -> anyhow::Result<Option<DaemonControlMessage>> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .map(Some)
                    .map_err(|error| anyhow::anyhow!("decode register message: {error}"));
            }
            Some(Ok(Message::Binary(bytes))) => {
                return serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|error| anyhow::anyhow!("decode register message: {error}"));
            }
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(Message::Ping(bytes))) => {
                socket.send(Message::Pong(bytes)).await?;
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

fn new_token() -> String {
    use base64::Engine as _;
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The public data route daemons dial for reverse data connections.
pub fn data_url(public_base_url: &str) -> String {
    format!(
        "{}{DATA_PATH}",
        crate::environments::gateway::websocket_base(public_base_url.trim_end_matches('/'))
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ed25519_dalek::{Signer as _, SigningKey};
    use environment_protocol::shared::{ImplementationInfo, SecretString};
    use environments::{
        BeginCloseEnvironment, CreateEnvironmentRegistrationKey, EnvironmentRegistrationKeyId,
        FinishCloseEnvironment, InMemoryEnvironmentRegistryStore, RegisteredIdentityMode,
        RegistrationKeyPolicy, RevokeEnvironmentRegistrationKey, mint_registration_key,
    };

    use super::*;

    fn daemon(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn register(key: &SigningKey, nonce: &[u8], secret: Option<&str>) -> RegisterParams {
        RegisterParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            registration_key: secret.map(SecretString::new),
            daemon_public_key: encode_hex(key.verifying_key().as_bytes()),
            signature: encode_hex(&key.sign(&signed_registration_message(nonce)).to_bytes()),
            display_name: Some("worker".to_owned()),
            metadata: BTreeMap::new(),
            implementation: ImplementationInfo {
                name: "lightspeed-envd".to_owned(),
                version: Some("0.1.0".to_owned()),
                git_sha: Some("1111111111111111111111111111111111111111".to_owned()),
                target: Some("x86_64-unknown-linux-musl".to_owned()),
            },
        }
    }

    async fn store_with_key(
        id: &str,
        policy: RegistrationKeyPolicy,
    ) -> (InMemoryEnvironmentRegistryStore, String) {
        let store = InMemoryEnvironmentRegistryStore::new();
        let minted = mint_registration_key(EnvironmentRegistrationKeyId::new(id), policy, 1_000)
            .expect("mint");
        store
            .create_registration_key(CreateEnvironmentRegistrationKey {
                secret_hash: minted.secret_hash.clone(),
                record: minted.record.clone(),
            })
            .await
            .expect("create key");
        (store, minted.secret.expose().to_owned())
    }

    /// Anchors recorded instead of stored: which environment ids were
    /// reserved and which released again.
    #[derive(Default)]
    struct RecordingAnchors {
        reserved: std::sync::Mutex<Vec<String>>,
        released: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl RegistrationAnchors for RecordingAnchors {
        async fn reserve(
            &self,
            _registration_key_id: &EnvironmentRegistrationKeyId,
            environment_id: &environments::EnvironmentId,
        ) -> Result<(), RegistrationRejection> {
            self.reserved
                .lock()
                .unwrap()
                .push(environment_id.to_string());
            Ok(())
        }

        async fn release(&self, environment_id: &environments::EnvironmentId) {
            self.released
                .lock()
                .unwrap()
                .push(environment_id.to_string());
        }
    }

    fn policy(mode: RegisteredIdentityMode) -> RegistrationKeyPolicy {
        RegistrationKeyPolicy {
            display_name: "pool".to_owned(),
            identity_mode: mode,
            max_active_environments: None,
            ephemeral_disconnect_grace_ms: None,
            expires_at_ms: None,
        }
    }

    #[test]
    fn register_requests_are_verified_against_the_nonce_and_domain() {
        let key = daemon(1);
        let nonce = [5u8; 32];
        assert!(verify_register_request(&register(&key, &nonce, None), &nonce).is_ok());
        let replayed = verify_register_request(&register(&key, &nonce, None), &[6u8; 32]);
        assert_eq!(
            replayed.unwrap_err().code,
            RegistrationRejectionCode::InvalidSignature
        );
        let mut wrong_key = register(&key, &nonce, None);
        wrong_key.daemon_public_key = encode_hex(daemon(2).verifying_key().as_bytes());
        assert_eq!(
            verify_register_request(&wrong_key, &nonce)
                .unwrap_err()
                .code,
            RegistrationRejectionCode::InvalidSignature
        );
        let mut old = register(&key, &nonce, None);
        old.protocol_version = 0;
        assert_eq!(
            verify_register_request(&old, &nonce).unwrap_err().code,
            RegistrationRejectionCode::UnsupportedProtocol
        );
        let mut reserved = register(&key, &nonce, None);
        reserved
            .metadata
            .insert("lightspeed.x".to_owned(), "y".to_owned());
        assert_eq!(
            verify_register_request(&reserved, &nonce).unwrap_err().code,
            RegistrationRejectionCode::InvalidRequest
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_creates_once_reconnects_by_identity_and_spends_closed_identities() {
        let (store, secret) = store_with_key("rk", policy(RegisteredIdentityMode::Ephemeral)).await;
        let anchors = RecordingAnchors::default();
        let key = daemon(3);
        let nonce = [1u8; 32];
        let unknown = admit_registration(&store, &anchors, &register(&key, &nonce, None), 2_000)
            .await
            .unwrap_err();
        assert_eq!(unknown.code, RegistrationRejectionCode::UnknownDaemon);

        let created = admit_registration(
            &store,
            &anchors,
            &register(&key, &nonce, Some(&secret)),
            2_000,
        )
        .await
        .expect("created");
        assert!(created.created);
        assert_eq!(created.environment.status, EnvironmentStatus::Ready);
        // The environment was anchored before it was created, and kept.
        assert_eq!(
            *anchors.reserved.lock().unwrap(),
            vec![created.environment.environment_id.to_string()]
        );
        assert!(anchors.released.lock().unwrap().is_empty());
        assert_eq!(
            created
                .environment
                .metadata
                .get("lightspeed.envd.version")
                .map(String::as_str),
            Some("0.1.0")
        );
        assert_eq!(
            created
                .environment
                .metadata
                .get("lightspeed.envd.gitSha")
                .map(String::as_str),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(
            created
                .environment
                .metadata
                .get("lightspeed.envd.protocolVersion")
                .map(String::as_str),
            Some(CURRENT_PROTOCOL_VERSION.to_string().as_str())
        );

        // Reconnects ignore the key entirely, even a wrong one.
        let reconnected = admit_registration(
            &store,
            &anchors,
            &register(&key, &nonce, Some("lsrk_wrong")),
            3_000,
        )
        .await
        .expect("reconnect");
        assert!(!reconnected.created);
        assert_eq!(
            reconnected.environment.environment_id,
            created.environment.environment_id
        );

        store
            .begin_close_environment(BeginCloseEnvironment {
                environment_id: created.environment.environment_id.clone(),
                updated_at_ms: 4_000,
            })
            .await
            .expect("close");
        let closing = admit_registration(
            &store,
            &anchors,
            &register(&key, &nonce, Some(&secret)),
            4_100,
        )
        .await
        .unwrap_err();
        assert_eq!(closing.code, RegistrationRejectionCode::EnvironmentClosed);
        store
            .finish_close_environment(FinishCloseEnvironment {
                environment_id: created.environment.environment_id.clone(),
                observed_at_ms: 4_200,
            })
            .await
            .expect("finish");
        let closed = admit_registration(
            &store,
            &anchors,
            &register(&key, &nonce, Some(&secret)),
            4_300,
        )
        .await
        .unwrap_err();
        assert_eq!(closed.code, RegistrationRejectionCode::EnvironmentClosed);
        assert!(closed.code.is_terminal());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_maps_key_policy_refusals_to_typed_codes() {
        let (store, secret) = store_with_key(
            "rk",
            RegistrationKeyPolicy {
                max_active_environments: Some(1),
                ..policy(RegisteredIdentityMode::Persistent)
            },
        )
        .await;
        let anchors = RecordingAnchors::default();
        let nonce = [2u8; 32];
        admit_registration(
            &store,
            &anchors,
            &register(&daemon(4), &nonce, Some(&secret)),
            2_000,
        )
        .await
        .expect("first");
        let full = admit_registration(
            &store,
            &anchors,
            &register(&daemon(5), &nonce, Some(&secret)),
            2_100,
        )
        .await
        .unwrap_err();
        assert_eq!(full.code, RegistrationRejectionCode::CapacityExhausted);
        assert!(!full.code.is_terminal());
        // A refused creation releases the anchor it reserved.
        let reserved = anchors.reserved.lock().unwrap().clone();
        assert_eq!(reserved.len(), 2);
        assert_eq!(*anchors.released.lock().unwrap(), reserved[1..].to_vec());

        let bad = admit_registration(
            &store,
            &anchors,
            &register(&daemon(6), &nonce, Some("lsrk_nope")),
            2_200,
        )
        .await
        .unwrap_err();
        assert_eq!(bad.code, RegistrationRejectionCode::InvalidRegistrationKey);

        store
            .revoke_registration_key(RevokeEnvironmentRegistrationKey {
                registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
                revoked_at_ms: 3_000,
            })
            .await
            .expect("revoke");
        let revoked = admit_registration(
            &store,
            &anchors,
            &register(&daemon(7), &nonce, Some(&secret)),
            3_100,
        )
        .await
        .unwrap_err();
        assert_eq!(
            revoked.code,
            RegistrationRejectionCode::RegistrationKeyRevoked
        );
        // The already registered daemon still reconnects after revocation.
        let reconnect =
            admit_registration(&store, &anchors, &register(&daemon(4), &nonce, None), 3_200)
                .await
                .expect("reconnect");
        assert!(!reconnect.created);

        let (expired_store, expired_secret) = store_with_key(
            "rk-exp",
            RegistrationKeyPolicy {
                expires_at_ms: Some(1_500),
                ..policy(RegisteredIdentityMode::Ephemeral)
            },
        )
        .await;
        let expired = admit_registration(
            &expired_store,
            &anchors,
            &register(&daemon(8), &nonce, Some(&expired_secret)),
            2_000,
        )
        .await
        .unwrap_err();
        assert_eq!(
            expired.code,
            RegistrationRejectionCode::RegistrationKeyExpired
        );
    }

    #[test]
    fn data_url_is_the_public_websocket_base_plus_the_data_path() {
        assert_eq!(
            data_url("https://lightspeed.example/"),
            "wss://lightspeed.example/environment-gateway/data"
        );
        assert_eq!(
            data_url("http://127.0.0.1:18080"),
            "ws://127.0.0.1:18080/environment-gateway/data"
        );
    }

    #[test]
    fn failure_accounting_rate_limits_a_subject() {
        let registrations = RegisteredConnections::new();
        for _ in 0..MAX_FAILURES_PER_WINDOW {
            registrations.record_failure("key:abc");
        }
        assert!(registrations.over_failure_limit("key:abc"));
        assert!(!registrations.over_failure_limit("key:other"));
    }
}
