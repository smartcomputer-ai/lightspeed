//! Outbound `envd` registration on the environment gateway.
//!
//! One public route accepts control connections: the registration channel
//! of `environment_protocol::registration`, on the shared JSON-RPC
//! envelope with the daemon as the client. The gateway sends the frozen
//! `registration/challenge` first; the daemon proves its identity in
//! `registration/register`, is admitted by a registration key the first
//! time and by its identity afterwards, and then holds the socket while the
//! gateway pings it and asks it, with `registration/openStream`, to dial
//! data streams. A second public route accepts those reverse-dialed stream
//! sockets and pairs each with the worker route that requested it; the
//! pair is then proxied frame for frame exactly like a passive external
//! environment. The gateway never names a URL: the daemon derives the
//! stream URL from its own connect URL.
//!
//! Liveness is written on transitions: admission and reconnect mark the
//! row connected, the end of the current connection marks it disconnected,
//! and while connected a coarse `last_seen_at_ms` stamp is written at most
//! every [`REGISTERED_LIVENESS_STAMP_MS`]. The registered sweep
//! ([`run_registered_sweep`]) flips rows whose stamp went stale, closes
//! ephemeral environments after their key's disconnect grace and
//! persistent ones after its `offlineCloseAfterMs`, and ends the routes of
//! environments closed elsewhere. The one close the session itself makes
//! is a departure: an ephemeral environment whose daemon said
//! `registration/goodbye` closes at once. A close in this process revokes
//! the environment's routes through [`EnvironmentSignals`]; its control
//! connection then re-reads its row and sends `registration/closed`.

use std::{
    collections::{HashMap, VecDeque},
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
use ed25519_dalek::{Signature, VerifyingKey};
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    bytes::SecretString,
    error::{BusyReason, NotFoundReason, PermissionReason, PreconditionReason, ProtocolError},
    ids::{ControlConnectionId, DaemonId, EnvironmentId},
    jsonrpc::{self, Id, Notification, Request},
    limits::{HEARTBEAT_MISSED_INTERVALS, MAX_MESSAGE_BYTES},
    manifest::{
        RegistrationChallenge, RegistrationClosed, RegistrationGoodbye, RegistrationOpenStream,
        RegistrationRegister,
    },
    metadata::DescriptiveText,
    registration::{
        Admission, ChallengeNonce, ChallengeParams, ClosedParams, ClosedReason, DaemonPublicKey,
        GoodbyeParams, OpenStreamParams, OpenStreamResponse, RegisterParams, RegisterResponse,
        RegistrationIdentityMode, RegistrationReceipt, StreamSpec, signed_registration_message,
    },
    routes::{CanonicalOrigin, DataRouteRefusal},
};
use environments::{
    BeginCloseEnvironment, CreateRegisteredEnvironment, DaemonBuild, EnvironmentCloseReason,
    EnvironmentRecord, EnvironmentRegistrationKeyStore, EnvironmentRegistryError, EnvironmentState,
    EnvironmentStore, MarkRegisteredConnected, MarkRegisteredDisconnected,
    REGISTERED_LIVENESS_STAMP_MS, RegisteredIdentityMode, RegisteredSweep, StampRegisteredSeen,
    WAIT_RETRY_AFTER_MS, registration_key_hash,
};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use uuid::Uuid;

use super::http::{GatewayState, refusal_response};
use crate::{
    environments::{
        gateway::{RouteKey, close_message},
        signals::EnvironmentSignals,
    },
    universe::UniverseRuntime,
};

/// Interval between gateway pings on a control connection. Sent to the
/// daemon as `heartbeatIntervalMs`; pongs keep the connection alive, and
/// at most one per [`REGISTERED_LIVENESS_STAMP_MS`] refreshes the row's
/// liveness stamp.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// A control connection that sends no frame for this long is dead.
const LIVENESS_TIMEOUT: Duration =
    Duration::from_secs(HEARTBEAT_INTERVAL.as_secs() * HEARTBEAT_MISSED_INTERVALS as u64);
/// Time from the challenge to the register request; the gateway closes
/// the socket when it passes and the daemon reconnects with backoff.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Time a daemon has to dial the stream route and answer one
/// `registration/openStream`. The worker's upgrade waits on this pairing,
/// so worker routes connect with a longer bound
/// (`environments::gateway::WORKER_ROUTE_CONNECT_TIMEOUT`) and receive the
/// gateway's 502/504 instead of their own connect timeout.
pub(crate) const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Pause before a control connection re-subscribes to its environment's
/// revocations after the subscription ended.
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
/// Interval of the registered sweep ([`run_registered_sweep`]).
pub const REGISTERED_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Unauthenticated handshakes in flight at once. A permit is held from the
/// upgrade until the admission is written, never for an admitted session.
const MAX_CONCURRENT_HANDSHAKES: usize = 256;
/// Failed admissions per verified daemon identity per window.
const MAX_FAILURES_PER_WINDOW: usize = 20;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// Daemon identities the failure limiter tracks at once; the identity whose
/// last failure is oldest is forgotten first.
const MAX_LIMITED_IDENTITIES: usize = 4096;
/// Registration frames are small JSON documents (a register request with
/// full metadata is about 12 KiB), so the control route accepts far less
/// than the protocol's message limit before anyone is authenticated.
const CONTROL_MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// `openStream` requests one control connection may have unanswered.
const CONTROL_COMMAND_QUEUE: usize = 16;

/// Live state of registered daemons on this gateway replica. In-memory by
/// design: durable truth is the environment row.
pub struct RegisteredConnections {
    controls: Mutex<HashMap<(Uuid, String), ControlHandle>>,
    /// One lock per environment with a connection transition in progress
    /// ([`RegisteredConnections::transition`]); pruned when the last holder
    /// or waiter is done.
    transitions: Mutex<HashMap<(Uuid, String), TransitionLock>>,
    pending: Mutex<HashMap<String, PendingStream>>,
    handshakes: Arc<Semaphore>,
    failures: Mutex<FailureLimiter>,
}

struct ControlHandle {
    connection_id: ControlConnectionId,
    commands: mpsc::Sender<ControlCommand>,
    /// Fired when a newer connection of the same identity is installed.
    /// A oneshot, so backpressure on `commands` can never drop it.
    supersede: oneshot::Sender<()>,
    /// Dropped with the handle: every data route paired under this
    /// connection subscribes to it and ends when it closes.
    ended: watch::Sender<()>,
}

enum ControlCommand {
    /// Ask the daemon for one data stream; `answered` receives its
    /// `registration/openStream` response.
    OpenStream {
        token: SecretString,
        answered: oneshot::Sender<Result<(), ProtocolError>>,
    },
}

/// A stream token the gateway issued and is waiting to pair.
struct PendingStream {
    sender: oneshot::Sender<WebSocket>,
    expires_at: Instant,
    key: RouteKey,
    /// The control connection the token was sent on; pairing is refused
    /// once it is no longer current.
    control_connection_id: ControlConnectionId,
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
            transitions: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            handshakes: Arc::new(Semaphore::new(MAX_CONCURRENT_HANDSHAKES)),
            failures: Mutex::new(FailureLimiter::default()),
        }
    }

    /// Serialize this replica's connection writes of one environment: an
    /// admission's install and its connected write, a session end's removal
    /// and its disconnected write, and a liveness stamp's "still current"
    /// check and its writes. A connection's disconnect therefore never lands
    /// after a newer connection's connected write, and a handle an admission
    /// hands back after a failed write is always removed again, with its
    /// disconnect, by its own session end.
    async fn transition(self: &Arc<Self>, universe_id: Uuid, environment_id: &str) -> Transition {
        let key = (universe_id, environment_id.to_owned());
        let lock = self
            .transitions
            .lock()
            .expect("registered transitions poisoned")
            .entry(key.clone())
            .or_default()
            .clone();
        let guard = lock.lock_owned().await;
        Transition {
            registrations: self.clone(),
            key,
            guard: Some(guard),
        }
    }

    fn current(&self, universe_id: Uuid, environment_id: &str) -> Option<ControlConnectionId> {
        self.controls
            .lock()
            .expect("registered connections poisoned")
            .get(&(universe_id, environment_id.to_owned()))
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
        connection_id: &ControlConnectionId,
    ) -> bool {
        self.uninstall(universe_id, environment_id, connection_id, None)
    }

    /// Replace the handle, if it is still the current one, with `previous`
    /// (an admission that failed hands the environment back to the
    /// connection it displaced) or with nothing; returns true when this
    /// connection was current until now.
    fn uninstall(
        &self,
        universe_id: Uuid,
        environment_id: &str,
        connection_id: &ControlConnectionId,
        previous: Option<ControlHandle>,
    ) -> bool {
        let mut controls = self
            .controls
            .lock()
            .expect("registered connections poisoned");
        let key = (universe_id, environment_id.to_owned());
        if !controls
            .get(&key)
            .is_some_and(|handle| &handle.connection_id == connection_id)
        {
            return false;
        }
        match previous {
            Some(previous) => controls.insert(key, previous),
            None => controls.remove(&key),
        };
        true
    }

    fn take_pending(&self, token: &str) -> Option<PendingStream> {
        let mut pending = self.pending.lock().expect("pending streams poisoned");
        let now = Instant::now();
        pending.retain(|_, entry| entry.expires_at > now);
        pending.remove(token)
    }

    fn retry_after(&self, identity: &DaemonPublicKey) -> Option<Duration> {
        self.failures
            .lock()
            .expect("failure limiter poisoned")
            .retry_after(identity, Instant::now())
    }

    fn record_failure(&self, identity: &DaemonPublicKey) {
        self.failures
            .lock()
            .expect("failure limiter poisoned")
            .record(*identity, Instant::now());
    }
}

type TransitionLock = Arc<tokio::sync::Mutex<()>>;

/// A held connection transition of one environment; the lock entry is
/// pruned on drop once nobody else holds or waits for it.
struct Transition {
    registrations: Arc<RegisteredConnections>,
    key: (Uuid, String),
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for Transition {
    fn drop(&mut self) {
        drop(self.guard.take());
        let Ok(mut transitions) = self.registrations.transitions.lock() else {
            return;
        };
        if transitions
            .get(&self.key)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            transitions.remove(&self.key);
        }
    }
}

/// Failed admissions per verified daemon identity. Keyed only on identities
/// whose signature verified, so nobody can spend another daemon's
/// allowance with claimed values; pruned on every insert and capped.
#[derive(Default)]
struct FailureLimiter {
    identities: HashMap<DaemonPublicKey, VecDeque<Instant>>,
}

impl FailureLimiter {
    /// How long `identity` must wait before its next attempt, when it is
    /// over the limit.
    fn retry_after(&self, identity: &DaemonPublicKey, now: Instant) -> Option<Duration> {
        let failures = self.identities.get(identity)?;
        let live: Vec<&Instant> = failures
            .iter()
            .filter(|at| now.duration_since(**at) < FAILURE_WINDOW)
            .collect();
        if live.len() < MAX_FAILURES_PER_WINDOW {
            return None;
        }
        let oldest = live.first()?;
        Some((FAILURE_WINDOW - now.duration_since(**oldest)).max(Duration::from_millis(1)))
    }

    fn record(&mut self, identity: DaemonPublicKey, now: Instant) {
        self.identities.retain(|_, failures| {
            while failures
                .front()
                .is_some_and(|at| now.duration_since(*at) >= FAILURE_WINDOW)
            {
                failures.pop_front();
            }
            !failures.is_empty()
        });
        if !self.identities.contains_key(&identity)
            && self.identities.len() >= MAX_LIMITED_IDENTITIES
            && let Some(stalest) = self
                .identities
                .iter()
                .min_by_key(|(_, failures)| failures.back().copied())
                .map(|(identity, _)| *identity)
        {
            self.identities.remove(&stalest);
        }
        let failures = self.identities.entry(identity).or_default();
        failures.push_back(now);
        while failures.len() > MAX_FAILURES_PER_WINDOW {
            failures.pop_front();
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.identities.len()
    }
}

/// Outcome of admitting one register request against durable state.
#[derive(Debug)]
pub struct AdmittedEnvironment {
    pub environment: EnvironmentRecord,
    pub admission: Admission,
}

/// How a verified register request will be admitted, decided before the
/// gateway installs the connection and writes the row.
#[derive(Debug)]
pub enum AdmissionPlan {
    /// A known daemon identity whose environment is open reconnects.
    Reconnect {
        environment: Box<EnvironmentRecord>,
        daemon_build: DaemonBuild,
    },
    /// A new identity that a registration key admits.
    Create(CreateRegisteredEnvironment),
}

impl AdmissionPlan {
    /// The environment the connection will serve.
    pub fn environment_id(&self) -> &environments::EnvironmentId {
        match self {
            Self::Reconnect { environment, .. } => &environment.environment_id,
            Self::Create(request) => &request.environment_id,
        }
    }
}

/// Decode `registration/register` params. The protocol version is read
/// first, so a daemon of another version learns it is stale even when the
/// rest of its request has a shape this gateway does not know; then out of
/// bounds descriptive values are `invalidRequest` and every other decode
/// failure `invalidParams`.
pub fn decode_register(params: &serde_json::Value) -> Result<RegisterParams, ProtocolError> {
    if let Some(version) = params
        .get("protocolVersion")
        .and_then(serde_json::Value::as_u64)
        && version != u64::from(ENVD_PROTOCOL_VERSION)
    {
        return Err(ProtocolError::version_mismatch(
            ENVD_PROTOCOL_VERSION,
            u32::try_from(version).unwrap_or(u32::MAX),
        ));
    }
    let params = RegisterParams::decode(params)?;
    if params.protocol_version != ENVD_PROTOCOL_VERSION {
        return Err(ProtocolError::version_mismatch(
            ENVD_PROTOCOL_VERSION,
            params.protocol_version,
        ));
    }
    Ok(params)
}

/// Check a register request's signature over the challenge nonce bound to
/// one of the gateway's accepted origins. Pure: no store access.
pub fn verify_register_request(
    params: &RegisterParams,
    nonce: &ChallengeNonce,
    origins: &[CanonicalOrigin],
) -> Result<(), ProtocolError> {
    let invalid = |message: &str| {
        ProtocolError::permission_denied(Some(PermissionReason::InvalidSignature), message)
    };
    let public_key = VerifyingKey::from_bytes(params.daemon_public_key.as_bytes())
        .map_err(|_| invalid("daemon public key is not a valid Ed25519 key"))?;
    let signature = Signature::from_bytes(params.signature.as_bytes());
    if origins.iter().any(|origin| {
        public_key
            .verify_strict(&signed_registration_message(nonce, origin), &signature)
            .is_ok()
    }) {
        return Ok(());
    }
    Err(invalid(
        "signature does not verify against the challenge and this gateway's origins",
    ))
}

/// Decide how a verified register request is admitted inside its universe:
/// a known daemon identity reconnects its open environment, a new one is
/// admitted through its registration key. Reads only; every refusal is
/// typed.
pub async fn plan_admission<S>(
    store: &S,
    params: &RegisterParams,
    now_ms: i64,
) -> Result<AdmissionPlan, ProtocolError>
where
    S: EnvironmentStore + EnvironmentRegistrationKeyStore + ?Sized,
{
    let daemon_public_key = params.daemon_public_key.to_hex();
    let daemon_build =
        DaemonBuild::from_implementation(&params.implementation, params.protocol_version);
    let known = store
        .read_environment_by_daemon_public_key(&daemon_public_key)
        .await
        .map_err(unavailable)?;
    if let Some(environment) = known {
        if environment.state == EnvironmentState::Closed {
            return Err(environment_closed(&environment));
        }
        return Ok(AdmissionPlan::Reconnect {
            environment: Box::new(environment),
            daemon_build,
        });
    }
    let Some(secret) = params.registration_key.as_ref() else {
        return Err(unknown_daemon());
    };
    let secret_hash = registration_key_hash(secret.expose());
    let key = store
        .resolve_registration_key(&secret_hash)
        .await
        .map_err(unavailable)?
        .ok_or_else(unknown_registration_key)?;
    key.check_admits(now_ms).map_err(map_registry_error)?;
    Ok(AdmissionPlan::Create(CreateRegisteredEnvironment {
        registration_key_id: key.registration_key_id.clone(),
        secret_hash,
        environment_id: super::service::allocate_environment_id_public(),
        daemon_public_key,
        display_name: params
            .display_name
            .as_ref()
            .map(|name| name.as_str().to_owned()),
        metadata: params.metadata.as_map().clone(),
        daemon_build,
        created_at_ms: now_ms,
    }))
}

/// Write an admission: mark a reconnecting environment connected with the
/// build that serves it now, or create the new environment open and
/// connected (the store re-checks the key and its capacity in the same
/// transaction). Nothing is left behind on a refusal.
pub async fn commit_admission<S>(
    store: &S,
    plan: AdmissionPlan,
    now_ms: i64,
) -> Result<AdmittedEnvironment, ProtocolError>
where
    S: EnvironmentStore + ?Sized,
{
    match plan {
        AdmissionPlan::Reconnect {
            environment,
            daemon_build,
        } => {
            let marked = store
                .mark_registered_connected(MarkRegisteredConnected {
                    environment_id: environment.environment_id.clone(),
                    daemon_build,
                    at_ms: now_ms,
                })
                .await;
            match marked {
                Ok(environment) => Ok(AdmittedEnvironment {
                    environment,
                    admission: Admission::Reconnected,
                }),
                // Closed since the plan read it: the identity is spent.
                Err(EnvironmentRegistryError::InvalidInput { .. })
                    if store
                        .read_environment(&environment.environment_id)
                        .await
                        .is_ok_and(|current| current.state == EnvironmentState::Closed) =>
                {
                    Err(environment_closed(&environment))
                }
                Err(error) => Err(map_registry_error(error)),
            }
        }
        AdmissionPlan::Create(request) => {
            let environment = store
                .create_registered_environment(request.clone())
                .await
                .map_err(map_registry_error)?;
            if environment.environment_id != request.environment_id {
                // Another connection of this identity was admitted first.
                return Err(if environment.state == EnvironmentState::Closed {
                    environment_closed(&environment)
                } else {
                    ProtocolError::unavailable(
                        "this daemon identity was admitted by another connection; reconnect",
                    )
                });
            }
            Ok(AdmittedEnvironment {
                environment,
                admission: Admission::Created,
            })
        }
    }
}

/// [`plan_admission`] then [`commit_admission`], for callers that hold no
/// connection to install in between.
pub async fn admit_registration<S>(
    store: &S,
    params: &RegisterParams,
    now_ms: i64,
) -> Result<AdmittedEnvironment, ProtocolError>
where
    S: EnvironmentStore + EnvironmentRegistrationKeyStore + ?Sized,
{
    let plan = plan_admission(store, params, now_ms).await?;
    commit_admission(store, plan, now_ms).await
}

/// The receipt of an admitted environment, built only from a registered
/// record.
fn registration_receipt(
    environment: &EnvironmentRecord,
    control_connection_id: &ControlConnectionId,
    admission: Admission,
) -> Result<RegistrationReceipt, ProtocolError> {
    let Some(source) = environment.registered() else {
        return Err(ProtocolError::internal(
            "the admitted environment is not a registered environment",
        ));
    };
    let internal = |error: environment_protocol::ids::IdError| {
        ProtocolError::internal(format!("admitted environment id: {error}"))
    };
    Ok(RegistrationReceipt {
        environment_id: EnvironmentId::new(environment.environment_id.as_str())
            .map_err(internal)?,
        daemon_id: DaemonId::new(source.daemon_id().as_str()).map_err(internal)?,
        control_connection_id: control_connection_id.clone(),
        identity_mode: match source.identity_mode {
            RegisteredIdentityMode::Persistent => RegistrationIdentityMode::Persistent,
            RegisteredIdentityMode::Ephemeral => RegistrationIdentityMode::Ephemeral,
        },
        admission,
    })
}

fn environment_closed(environment: &EnvironmentRecord) -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::EnvironmentClosed,
        format!(
            "environment {} is closed; this daemon identity is spent",
            environment.environment_id
        ),
    )
}

fn unknown_daemon() -> ProtocolError {
    ProtocolError::not_found_because(
        NotFoundReason::UnknownDaemon,
        "unknown daemon identity and no registration key presented",
    )
}

fn unknown_registration_key() -> ProtocolError {
    ProtocolError::permission_denied(
        Some(PermissionReason::InvalidRegistrationKey),
        "registration key is not recognized",
    )
}

fn map_registry_error(error: EnvironmentRegistryError) -> ProtocolError {
    match error {
        EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "expired", ..
        } => ProtocolError::expired("registration key is expired"),
        // A rotation retired the presented secret while its admission was in
        // flight: the same answer a later admission with it gets.
        EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "rotated", ..
        } => unknown_registration_key(),
        EnvironmentRegistryError::RegistrationKeyUnavailable { reason, .. } => {
            ProtocolError::permission_denied(
                Some(PermissionReason::RegistrationKeyRevoked),
                format!("registration key is {reason}"),
            )
        }
        EnvironmentRegistryError::RegistrationCapacityExhausted { limit, .. } => {
            ProtocolError::busy(
                Some(BusyReason::CapacityExhausted),
                None,
                format!("registration key has reached its active environment limit of {limit}"),
            )
        }
        EnvironmentRegistryError::AlreadyExists {
            kind: "daemon_identity",
            ..
        } => ProtocolError::failed_precondition(
            PreconditionReason::IdentityInUse,
            "daemon identity is already bound to an environment elsewhere",
        ),
        // The record the request would create breaks the registry's bounds:
        // admission is refused, never granted with the value dropped.
        EnvironmentRegistryError::InvalidInput { message } => {
            ProtocolError::invalid_request(message)
        }
        other => unavailable(other),
    }
}

fn unavailable(error: EnvironmentRegistryError) -> ProtocolError {
    tracing::warn!(target: "temporal_runtime", %error, "registration store failure");
    registration_unavailable()
}

fn registration_unavailable() -> ProtocolError {
    ProtocolError::unavailable("registration is temporarily unavailable")
}

/// True for refusals that count against a verified identity's allowance:
/// credentials that did not admit it.
fn counts_as_failure(error: &ProtocolError) -> bool {
    use environment_protocol::error::ErrorData;
    matches!(
        error.data,
        ErrorData::PermissionDenied { .. }
            | ErrorData::Expired {}
            | ErrorData::NotFound {
                reason: Some(NotFoundReason::UnknownDaemon)
            }
    )
}

/// `GET /environment-gateway/connect`: the daemon's control connection.
pub(super) async fn connect_upgrade(
    State(state): State<Arc<GatewayState>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if state.registration_origins().is_empty() {
        tracing::error!(
            target: "temporal_runtime",
            "registration refused: the gateway has no valid origin to verify daemon signatures against"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Ok(permit) = state.registrations().handshakes.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    upgrade
        .max_message_size(CONTROL_MAX_MESSAGE_BYTES)
        .max_frame_size(CONTROL_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| control_connection(state, socket, permit))
        .into_response()
}

/// `GET /environment-gateway/data`: a reverse-dialed stream socket
/// presenting its one-time token as a bearer header.
pub(super) async fn data_upgrade(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let registrations = state.registrations();
    let Some(pending) = registrations.take_pending(token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if registrations.current(pending.key.universe_id, &pending.key.environment_id)
        != Some(pending.control_connection_id.clone())
    {
        // The control connection that issued the token was superseded or
        // ended: whoever dials now is not the daemon it was sent to.
        return StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES as usize)
        .max_frame_size(MAX_MESSAGE_BYTES as usize)
        .on_upgrade(move |socket| async move {
            // The waiting route owns the socket from here; if it gave up,
            // dropping the socket closes the daemon's dial.
            let _ = pending.sender.send(socket);
        })
        .into_response()
}

/// Ask the daemon serving `key` to dial a data stream for one worker route
/// and wait for it. Returns the paired socket and a receiver that closes
/// when the control connection it was issued under ends. Without a live
/// control connection the route is refused `disconnected`; a daemon that
/// cannot open the stream is a bodiless 502, one that does not dial within
/// [`STREAM_OPEN_TIMEOUT`] a 504.
pub(super) async fn open_registered_route(
    state: &Arc<GatewayState>,
    key: &RouteKey,
) -> Result<(WebSocket, watch::Receiver<()>), Box<Response>> {
    let disconnected = || {
        Box::new(refusal_response(
            &DataRouteRefusal::Disconnected {},
            Some(WAIT_RETRY_AFTER_MS),
        ))
    };
    let registrations = state.registrations();
    let (commands, control_connection_id, control_ended) = {
        let controls = registrations
            .controls
            .lock()
            .expect("registered connections poisoned");
        let Some(handle) = controls.get(&(key.universe_id, key.environment_id.clone())) else {
            return Err(disconnected());
        };
        (
            handle.commands.clone(),
            handle.connection_id.clone(),
            handle.ended.subscribe(),
        )
    };
    let token = new_token();
    let (sender, socket) = oneshot::channel();
    registrations
        .pending
        .lock()
        .expect("pending streams poisoned")
        .insert(
            token.clone(),
            PendingStream {
                sender,
                expires_at: Instant::now() + STREAM_OPEN_TIMEOUT,
                key: key.clone(),
                control_connection_id,
            },
        );
    let opened = tokio::time::timeout(STREAM_OPEN_TIMEOUT, async {
        let (answered, answer) = oneshot::channel();
        commands
            .send(ControlCommand::OpenStream {
                token: SecretString::new(token.clone()),
                answered,
            })
            .await
            .map_err(|_| disconnected())?;
        match answer.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "temporal_runtime",
                    environment = %key.environment_id,
                    %error,
                    "registered daemon could not open a stream"
                );
                return Err(Box::new(StatusCode::BAD_GATEWAY.into_response()));
            }
            // The control connection ended before the daemon answered.
            Err(_) => return Err(disconnected()),
        }
        socket
            .await
            .map_err(|_| Box::new(StatusCode::BAD_GATEWAY.into_response()))
    })
    .await;
    match opened {
        Ok(Ok(socket)) => Ok((socket, control_ended)),
        Ok(Err(refused)) => {
            registrations.take_pending(&token);
            Err(refused)
        }
        Err(_) => {
            registrations.take_pending(&token);
            Err(Box::new(StatusCode::GATEWAY_TIMEOUT.into_response()))
        }
    }
}

/// Pipe one worker route and one reverse-dialed daemon socket until either
/// side ends, the environment's routes are revoked, or the control
/// connection the stream was issued under ends.
pub(super) async fn proxy_registered_route(
    mut revoked: watch::Receiver<u64>,
    mut control_ended: watch::Receiver<()>,
    mut worker: WebSocket,
    mut daemon: WebSocket,
) {
    loop {
        tokio::select! {
            // A dropped revocation sender fails closed like a revocation.
            _ = revoked.changed() => {
                let _ = worker.send(close_message("environment route revoked")).await;
                break;
            }
            // Nobody sends on it: it resolves when the handle is dropped.
            _ = control_ended.changed() => {
                let _ = worker.send(close_message("registered control connection ended")).await;
                break;
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

/// What the gateway does with one frame before admission.
#[derive(Debug, PartialEq)]
enum PreAdmission {
    /// A `registration/register` request.
    Register(Request),
    /// Answer and keep waiting: a framing error.
    Answer(jsonrpc::Response),
    /// Answer, then close: a request other than register.
    AnswerAndClose(jsonrpc::Response),
    /// Close without an answer: a notification before admission.
    Close,
    /// Nothing to do: a response to an id the gateway never sent, a
    /// malformed notification, a ping or pong.
    Ignore,
}

fn classify_pre_admission(message: &Message) -> PreAdmission {
    match message {
        Message::Text(text) => match jsonrpc::parse(text.as_str()) {
            Err(rejection) => match rejection.into_response() {
                Some(response) => PreAdmission::Answer(response),
                None => PreAdmission::Ignore,
            },
            Ok(jsonrpc::Message::Request(request))
                if request.method == RegistrationRegister::NAME =>
            {
                PreAdmission::Register(request)
            }
            Ok(jsonrpc::Message::Request(request)) => PreAdmission::AnswerAndClose(
                jsonrpc::Response::failure(Some(request.id), ProtocolError::not_initialized()),
            ),
            Ok(jsonrpc::Message::Notification(_)) => PreAdmission::Close,
            Ok(jsonrpc::Message::Response(_)) => PreAdmission::Ignore,
        },
        Message::Binary(_) => PreAdmission::Answer(binary_frame_refusal()),
        Message::Close(_) => PreAdmission::Close,
        Message::Ping(_) | Message::Pong(_) => PreAdmission::Ignore,
    }
}

/// What the gateway does with one text frame after admission.
#[derive(Debug, PartialEq)]
enum Admitted {
    /// Send this answer; the connection stays open.
    Answer(jsonrpc::Response),
    /// The daemon answered the gateway request with this id.
    StreamAnswer(Id, Result<(), ProtocolError>),
    /// `registration/goodbye`: the daemon is stopping deliberately.
    Goodbye,
    /// Ignore and log.
    Ignore(String),
}

fn classify_admitted(text: &str) -> Admitted {
    match jsonrpc::parse(text) {
        Err(rejection) => match rejection.into_response() {
            Some(response) => Admitted::Answer(response),
            None => Admitted::Ignore("malformed notification".to_owned()),
        },
        Ok(jsonrpc::Message::Request(request)) => {
            let error = if request.method == RegistrationRegister::NAME {
                ProtocolError::invalid_request("the daemon is already admitted")
            } else {
                ProtocolError::method_not_found(&request.method)
            };
            Admitted::Answer(jsonrpc::Response::failure(Some(request.id), error))
        }
        Ok(jsonrpc::Message::Notification(notification))
            if notification.method == RegistrationGoodbye::NAME =>
        {
            match notification.params::<GoodbyeParams>() {
                Ok(_) => Admitted::Goodbye,
                Err(error) => Admitted::Ignore(format!("undecodable goodbye: {error}")),
            }
        }
        Ok(jsonrpc::Message::Notification(notification)) => {
            Admitted::Ignore(format!("unknown notification {}", notification.method))
        }
        Ok(jsonrpc::Message::Response(response)) => match response.id.clone() {
            Some(id) => {
                let answer = response
                    .decode::<OpenStreamResponse>()
                    .map(|_| ())
                    .map_err(|error| match error {
                        jsonrpc::ResponseError::Failed(error) => error,
                        jsonrpc::ResponseError::Undecodable(message) => ProtocolError::internal(
                            format!("the openStream response breaks the contract: {message}"),
                        ),
                    });
                Admitted::StreamAnswer(id, answer)
            }
            None => Admitted::Ignore(format!(
                "error response without an id: {}",
                response
                    .result
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_default()
            )),
        },
    }
}

fn binary_frame_refusal() -> jsonrpc::Response {
    jsonrpc::Response::failure(
        None,
        ProtocolError::invalid_request("binary frames are not part of the registration channel"),
    )
}

/// A register request whose signature verified, with its admission decided
/// but not yet written.
struct Verified {
    universe_id: Uuid,
    store: Arc<store_pg::PgStore>,
    register: RegisterParams,
    register_id: Id,
    plan: AdmissionPlan,
}

/// An admission that was written, with the installed connection's
/// channels.
struct AdmittedConnection {
    universe_id: Uuid,
    store: Arc<store_pg::PgStore>,
    register: RegisterParams,
    register_id: Id,
    environment: EnvironmentRecord,
    receipt: RegistrationReceipt,
    connection_id: ControlConnectionId,
    commands: mpsc::Receiver<ControlCommand>,
    superseded: oneshot::Receiver<()>,
    revoked: watch::Receiver<u64>,
}

/// Why a handshake produced no admitted session.
enum Refused {
    /// The socket is gone or the daemon gave up; nothing to answer.
    Silent,
    /// Answer the register request with this error, then close.
    Answer(Id, ProtocolError),
}

async fn control_connection(
    state: Arc<GatewayState>,
    mut socket: WebSocket,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    // The handshake waits on the daemon and is bounded; the admission write
    // that follows runs to completion, so a row is never left connected
    // without the connection that wrote it.
    let verified =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&state, &mut socket)).await {
            Ok(verified) => verified,
            Err(_) => {
                // A daemon that is paused or starved retries with backoff.
                tracing::info!(target: "temporal_runtime", "registration handshake timed out");
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        };
    let admitted = match verified {
        Ok(verified) => admit(&state, verified).await,
        Err(refused) => Err(refused),
    };
    // The permit bounds unauthenticated handshakes only: it is released as
    // soon as the admission resolved, whatever its outcome.
    drop(permit);
    match admitted {
        Ok(admitted) => {
            if let Err(error) = admitted_session(&state, socket, admitted).await {
                tracing::debug!(target: "temporal_runtime", %error, "registration control session ended with error");
            }
        }
        Err(Refused::Answer(id, error)) => {
            tracing::info!(target: "temporal_runtime", kind = error.kind().as_str(), %error, "registration refused");
            let _ = send_text(
                &mut socket,
                jsonrpc::Response::failure(Some(id), error).to_text(),
            )
            .await;
            let _ = socket.send(Message::Close(None)).await;
        }
        Err(Refused::Silent) => {
            let _ = socket.send(Message::Close(None)).await;
        }
    }
}

/// Challenge, read the register request, verify it and decide its
/// admission.
async fn handshake(state: &Arc<GatewayState>, socket: &mut WebSocket) -> Result<Verified, Refused> {
    let nonce = new_nonce();
    let challenge = Notification::new(
        RegistrationChallenge::NAME,
        &ChallengeParams {
            protocol_version: ENVD_PROTOCOL_VERSION,
            nonce,
        },
    )
    .map_err(|_| Refused::Silent)?;
    send_text(socket, challenge.to_text())
        .await
        .map_err(|_| Refused::Silent)?;
    let request = loop {
        let Some(Ok(message)) = socket.recv().await else {
            return Err(Refused::Silent);
        };
        match classify_pre_admission(&message) {
            PreAdmission::Register(request) => break request,
            PreAdmission::Answer(response) => {
                send_text(socket, response.to_text())
                    .await
                    .map_err(|_| Refused::Silent)?;
            }
            PreAdmission::AnswerAndClose(response) => {
                let _ = send_text(socket, response.to_text()).await;
                return Err(Refused::Silent);
            }
            PreAdmission::Close => return Err(Refused::Silent),
            PreAdmission::Ignore => {}
        }
    };
    let id = request.id.clone();
    let refuse = |error: ProtocolError| Refused::Answer(id.clone(), error);
    let register = decode_register(&request.params).map_err(refuse)?;
    verify_register_request(&register, &nonce, state.registration_origins()).map_err(refuse)?;

    // From here the identity is verified: rate limits key on it alone.
    let registrations = state.registrations();
    let identity = register.daemon_public_key;
    if let Some(retry_after) = registrations.retry_after(&identity) {
        return Err(refuse(ProtocolError::busy(
            Some(BusyReason::RateLimited),
            Some(u64::try_from(retry_after.as_millis()).unwrap_or(u64::MAX)),
            "too many failed registrations for this daemon identity; retry later",
        )));
    }
    let refuse_counted = |error: ProtocolError| {
        if counts_as_failure(&error) {
            registrations.record_failure(&identity);
        }
        refuse(error)
    };
    let universe_id = match state.registration_universe(&register).await {
        Ok(Some(universe_id)) => universe_id,
        Ok(None) => {
            return Err(refuse_counted(if register.registration_key.is_some() {
                unknown_registration_key()
            } else {
                unknown_daemon()
            }));
        }
        Err(error) => {
            tracing::warn!(target: "temporal_runtime", error = %error.message, "registration universe lookup failed");
            return Err(refuse(registration_unavailable()));
        }
    };
    let api = state.api_for_daemon(universe_id).await.map_err(|error| {
        tracing::warn!(target: "temporal_runtime", error = %error.message, "registration universe unavailable");
        refuse(registration_unavailable())
    })?;
    let store = api.store().clone();
    let plan = plan_admission(store.as_ref(), &register, now_ms())
        .await
        .map_err(refuse_counted)?;
    Ok(Verified {
        universe_id,
        store,
        register,
        register_id: id,
        plan,
    })
}

/// Install the connection, then write the admission, both inside the
/// environment's connection transition. The handle goes in first, so a row
/// that reads connected without a handle exists only while a connection
/// ends. The connection it displaces is superseded only once the write
/// succeeded; a refused admission hands the environment back to it, and
/// since that connection's own session end takes the same transition, a
/// handle handed back after its session ended is removed again with its
/// disconnect.
async fn admit(
    state: &Arc<GatewayState>,
    verified: Verified,
) -> Result<AdmittedConnection, Refused> {
    let Verified {
        universe_id,
        store,
        register,
        register_id,
        plan,
    } = verified;
    let registrations = state.registrations();
    let environment_id = plan.environment_id().to_string();
    let connection_id = ControlConnectionId::new(format!("connection_{}", Uuid::new_v4().simple()))
        .expect("generated control connection ids are valid");
    let (commands, command_rx) = mpsc::channel(CONTROL_COMMAND_QUEUE);
    let (supersede, superseded) = oneshot::channel();
    let (ended, _) = watch::channel(());
    // Subscribed before the write: a close that lands after it still
    // reaches this connection.
    let revoked = state.signals().watch(universe_id, &environment_id);
    let _transition = registrations.transition(universe_id, &environment_id).await;
    let previous = registrations.install(
        universe_id,
        environment_id.clone(),
        ControlHandle {
            connection_id: connection_id.clone(),
            commands,
            supersede,
            ended,
        },
    );
    let refuse = |error: ProtocolError| {
        if counts_as_failure(&error) {
            registrations.record_failure(&register.daemon_public_key);
        }
        Refused::Answer(register_id.clone(), error)
    };
    let admitted = match commit_admission(store.as_ref(), plan, now_ms()).await {
        Ok(admitted) => admitted,
        Err(error) => {
            registrations.uninstall(universe_id, &environment_id, &connection_id, previous);
            return Err(refuse(error));
        }
    };
    let receipt =
        match registration_receipt(&admitted.environment, &connection_id, admitted.admission) {
            Ok(receipt) => receipt,
            Err(error) => {
                // A handed-back connection is live and keeps the row
                // connected; without one nobody holds it.
                let handed_back = previous.is_some();
                if registrations.uninstall(universe_id, &environment_id, &connection_id, previous)
                    && !handed_back
                {
                    mark_disconnected(&store, &admitted.environment.environment_id).await;
                }
                return Err(refuse(error));
            }
        };
    if let Some(previous) = previous {
        // Dropping the displaced handle also ends its data routes.
        let _ = previous.supersede.send(());
    }
    Ok(AdmittedConnection {
        universe_id,
        store,
        register,
        register_id,
        environment: admitted.environment,
        receipt,
        connection_id,
        commands: command_rx,
        superseded,
        revoked,
    })
}

/// The admitted control connection: answer the register request, then
/// ping, pair streams and watch for revocation until the session ends.
/// Cleanup runs on every exit path.
async fn admitted_session(
    state: &Arc<GatewayState>,
    mut socket: WebSocket,
    admitted: AdmittedConnection,
) -> anyhow::Result<()> {
    let AdmittedConnection {
        universe_id,
        store,
        register,
        register_id,
        environment,
        receipt,
        connection_id,
        commands,
        superseded,
        revoked,
    } = admitted;
    let environment_id = environment.environment_id.to_string();
    let registrations = state.registrations();
    tracing::info!(
        target: "temporal_runtime",
        %universe_id,
        environment = %environment_id,
        daemon = %receipt.daemon_id,
        daemon_public_key = %register.daemon_public_key,
        connection = %connection_id,
        admission = ?receipt.admission,
        "registered environment control connection admitted"
    );
    let session = ControlSession {
        state,
        store: &store,
        universe_id,
        environment: &environment,
        connection_id: &connection_id,
        daemon_build: DaemonBuild::from_implementation(
            &register.implementation,
            register.protocol_version,
        ),
    };
    let end = session
        .serve(
            &mut socket,
            register_id,
            receipt,
            commands,
            superseded,
            revoked,
        )
        .await;
    // Inside the transition: a liveness write that already passed its
    // "still current" check lands first, a newer admission's connected
    // write lands after this disconnect, and a later liveness write sees
    // the removal.
    let transition = registrations.transition(universe_id, &environment_id).await;
    let was_current = registrations.remove_if_current(universe_id, &environment_id, &connection_id);
    tracing::info!(
        target: "temporal_runtime",
        %universe_id,
        environment = %environment_id,
        connection = %connection_id,
        reason = end.reason(),
        current = was_current,
        "registered environment control connection ended"
    );
    if was_current {
        mark_disconnected(&store, &environment.environment_id).await;
        if end == SessionEnd::Goodbye && departure_closes(&environment) {
            close_departed(&store, &environment).await;
            state.signals().revoke(universe_id, &environment_id);
        }
    }
    drop(transition);
    let _ = socket.send(Message::Close(None)).await;
    Ok(())
}

/// A deliberate goodbye ends an ephemeral daemon's sandbox for good, so its
/// environment closes at once instead of after the key's disconnect grace.
/// A persistent daemon's environment waits for it to return.
fn departure_closes(environment: &EnvironmentRecord) -> bool {
    environment
        .registered()
        .is_some_and(|source| source.identity_mode == RegisteredIdentityMode::Ephemeral)
}

async fn mark_disconnected(
    store: &Arc<store_pg::PgStore>,
    environment_id: &environments::EnvironmentId,
) {
    let result = EnvironmentStore::mark_registered_disconnected(
        store.as_ref(),
        MarkRegisteredDisconnected {
            environment_id: environment_id.clone(),
            at_ms: now_ms(),
        },
    )
    .await;
    // The registered sweep marks the row disconnected once its liveness
    // stamp goes stale.
    if let Err(error) = result {
        tracing::warn!(target: "temporal_runtime", %error, environment = %environment_id, "recording a registered disconnect failed");
    }
}

async fn close_departed(store: &Arc<store_pg::PgStore>, environment: &EnvironmentRecord) {
    let result = EnvironmentStore::begin_close_environment(
        store.as_ref(),
        BeginCloseEnvironment {
            environment_id: environment.environment_id.clone(),
            reason: EnvironmentCloseReason::Departed,
            at_ms: now_ms(),
        },
    )
    .await;
    match result {
        Ok(_) => tracing::info!(
            target: "temporal_runtime",
            environment = %environment.environment_id,
            "ephemeral registered daemon departed; closed its environment"
        ),
        // The registered sweep closes it after the key's grace.
        Err(error) => tracing::warn!(
            target: "temporal_runtime",
            environment = %environment.environment_id,
            %error,
            "closing a departed ephemeral environment failed"
        ),
    }
}

/// Why an admitted session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionEnd {
    Superseded,
    EnvironmentClosed,
    Goodbye,
    HeartbeatTimeout,
    DaemonClosed,
    SendFailed,
}

impl SessionEnd {
    fn reason(self) -> &'static str {
        match self {
            Self::Superseded => "superseded",
            Self::EnvironmentClosed => "environment closed",
            Self::Goodbye => "goodbye",
            Self::HeartbeatTimeout => "heartbeat timeout",
            Self::DaemonClosed => "daemon closed",
            Self::SendFailed => "send failed",
        }
    }
}

struct ControlSession<'a> {
    state: &'a Arc<GatewayState>,
    store: &'a Arc<store_pg::PgStore>,
    universe_id: Uuid,
    environment: &'a EnvironmentRecord,
    connection_id: &'a ControlConnectionId,
    daemon_build: DaemonBuild,
}

impl ControlSession<'_> {
    /// Serve the admitted session until it ends.
    async fn serve(
        &self,
        socket: &mut WebSocket,
        register_id: Id,
        receipt: RegistrationReceipt,
        commands: mpsc::Receiver<ControlCommand>,
        superseded: oneshot::Receiver<()>,
        revoked: watch::Receiver<u64>,
    ) -> SessionEnd {
        let response = jsonrpc::Response::success(
            register_id,
            &RegisterResponse {
                receipt,
                heartbeat_interval_ms: u64::try_from(HEARTBEAT_INTERVAL.as_millis())
                    .unwrap_or(u64::MAX),
            },
        );
        if send_text(socket, response.to_text()).await.is_err() {
            return SessionEnd::SendFailed;
        }
        self.run(socket, commands, superseded, revoked).await
    }

    async fn run(
        &self,
        socket: &mut WebSocket,
        mut commands: mpsc::Receiver<ControlCommand>,
        mut superseded: oneshot::Receiver<()>,
        revoked: watch::Receiver<u64>,
    ) -> SessionEnd {
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let (mut environment_closed, _watcher) = self.watch_environment_closed(revoked);
        let mut watching = true;
        let mut deadline = tokio::time::Instant::now() + LIVENESS_TIMEOUT;
        // The admission wrote the first stamp.
        let mut last_stamp_ms = self
            .environment
            .registered()
            .map_or_else(now_ms, |source| source.last_seen_at_ms);
        let mut next_request = 0_u64;
        let mut streams: HashMap<Id, oneshot::Sender<Result<(), ProtocolError>>> = HashMap::new();
        let mut commands_closed = false;
        // One liveness write at a time. One still running when the session
        // ends finishes on its own: it holds the environment's transition,
        // so the disconnect waits for it.
        let mut seen_write: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            // Biased: a frame that already arrived is read before the
            // liveness deadline is checked, so a loop that fell behind
            // never declares an answering daemon dead.
            tokio::select! {
                biased;
                _ = &mut superseded => {
                    self.closed(socket, ClosedReason::Superseded, "another process with this daemon identity connected").await;
                    return SessionEnd::Superseded;
                }
                closed = &mut environment_closed, if watching => {
                    if closed.is_ok() {
                        self.closed(socket, ClosedReason::EnvironmentClosed, "the environment was closed").await;
                        return SessionEnd::EnvironmentClosed;
                    }
                    watching = false;
                }
                command = commands.recv(), if !commands_closed => {
                    // The channel closes only when a newer connection
                    // replaced this session's handle; supersede follows.
                    let Some(ControlCommand::OpenStream { token, answered }) = command else {
                        commands_closed = true;
                        continue;
                    };
                    next_request += 1;
                    let id = Id::String(format!("g{next_request}"));
                    let request = Request::new(
                        id.clone(),
                        RegistrationOpenStream::NAME,
                        &OpenStreamParams { token, stream: StreamSpec::DataProtocol {} },
                    );
                    let Ok(request) = request else {
                        let _ = answered.send(Err(ProtocolError::internal("encode openStream")));
                        continue;
                    };
                    if send_text(socket, request.to_text()).await.is_err() {
                        return SessionEnd::SendFailed;
                    }
                    streams.retain(|_, waiting| !waiting.is_closed());
                    streams.insert(id, answered);
                }
                _ = heartbeat.tick() => {
                    if socket.send(Message::Ping(Default::default())).await.is_err() {
                        return SessionEnd::SendFailed;
                    }
                }
                message = socket.recv() => {
                    let Some(Ok(message)) = message else {
                        return SessionEnd::DaemonClosed;
                    };
                    deadline = tokio::time::Instant::now() + LIVENESS_TIMEOUT;
                    let answer = match message {
                        Message::Pong(_) => {
                            let at_ms = now_ms();
                            if at_ms.saturating_sub(last_stamp_ms) >= REGISTERED_LIVENESS_STAMP_MS
                                && seen_write.as_ref().is_none_or(|write| write.is_finished())
                            {
                                last_stamp_ms = at_ms;
                                seen_write = Some(self.record_seen(at_ms));
                            }
                            None
                        }
                        Message::Ping(_) => None,
                        Message::Close(_) => return SessionEnd::DaemonClosed,
                        Message::Binary(_) => Some(binary_frame_refusal()),
                        Message::Text(text) => match classify_admitted(text.as_str()) {
                            Admitted::Answer(response) => Some(response),
                            Admitted::StreamAnswer(id, answer) => {
                                match streams.remove(&id) {
                                    Some(waiting) => { let _ = waiting.send(answer); }
                                    None => tracing::debug!(target: "temporal_runtime", %id, "ignoring a response to an unknown request id"),
                                }
                                None
                            }
                            Admitted::Goodbye => {
                                tracing::info!(target: "temporal_runtime", environment = %self.environment.environment_id, "registered daemon said goodbye");
                                return SessionEnd::Goodbye;
                            }
                            Admitted::Ignore(reason) => {
                                tracing::debug!(target: "temporal_runtime", %reason, "ignoring a registration frame");
                                None
                            }
                        },
                    };
                    if let Some(answer) = answer
                        && send_text(socket, answer.to_text()).await.is_err()
                    {
                        return SessionEnd::SendFailed;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return SessionEnd::HeartbeatTimeout;
                }
            }
        }
    }

    /// Wait off the session loop for the environment's routes to be
    /// revoked, then re-read the row once, so a slow store never delays
    /// pings, pongs or stream requests. The receiver fires once the
    /// environment is closed; the watcher stops when the returned guard
    /// drops.
    fn watch_environment_closed(
        &self,
        mut revoked: watch::Receiver<u64>,
    ) -> (oneshot::Receiver<()>, AbortOnDrop) {
        let signals: Arc<EnvironmentSignals> = self.state.signals().clone();
        let store = self.store.clone();
        let universe_id = self.universe_id;
        let environment_id = self.environment.environment_id.clone();
        let (closed, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                if revoked.changed().await.is_err() {
                    // The subscription ended without a revocation: take a
                    // new one, then read the row in case a close was missed.
                    tokio::time::sleep(RESUBSCRIBE_DELAY).await;
                    revoked = signals.watch(universe_id, environment_id.as_str());
                }
                match EnvironmentStore::read_environment(store.as_ref(), &environment_id).await {
                    Ok(current) if current.state == EnvironmentState::Closed => {
                        let _ = closed.send(());
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(target: "temporal_runtime", %error, environment = %environment_id, "registered control connection could not re-read its environment");
                    }
                }
            }
        });
        (receiver, AbortOnDrop(task))
    }

    /// Tell the daemon its session ends for good, then close.
    async fn closed(&self, socket: &mut WebSocket, reason: ClosedReason, message: &str) {
        if let Ok(notification) = Notification::new(
            RegistrationClosed::NAME,
            &ClosedParams {
                reason,
                message: DescriptiveText::lossy(message),
            },
        ) {
            let _ = send_text(socket, notification.to_text()).await;
        }
        let _ = socket
            .send(close_message(match reason {
                ClosedReason::Superseded => "superseded by a newer connection of the same daemon",
                ClosedReason::EnvironmentClosed => "environment closed",
            }))
            .await;
    }

    /// The coarse liveness stamp of the current control connection, written
    /// off the session loop so a slow store never delays pings or stream
    /// requests, inside the environment's connection transition. When the
    /// row no longer reads connected (a stale sweep or a lost write marked
    /// it disconnected while this connection was up), the connection is
    /// recorded again instead.
    fn record_seen(&self, at_ms: i64) -> tokio::task::JoinHandle<()> {
        let registrations = self.state.registrations().clone();
        let store = self.store.clone();
        let universe_id = self.universe_id;
        let environment_id = self.environment.environment_id.clone();
        let connection_id = self.connection_id.clone();
        let daemon_build = self.daemon_build.clone();
        tokio::spawn(async move {
            let _transition = registrations
                .transition(universe_id, environment_id.as_str())
                .await;
            if registrations.current(universe_id, environment_id.as_str()) != Some(connection_id) {
                return;
            }
            let stamped = EnvironmentStore::stamp_registered_seen(
                store.as_ref(),
                StampRegisteredSeen {
                    environment_id: environment_id.clone(),
                    at_ms,
                },
            )
            .await;
            let restore = match stamped {
                Ok(true) => false,
                Ok(false) => EnvironmentStore::read_environment(store.as_ref(), &environment_id)
                    .await
                    .is_ok_and(|current| {
                        current.state == EnvironmentState::Open
                            && current.registered().is_some_and(|source| !source.connected)
                    }),
                Err(error) => {
                    tracing::warn!(target: "temporal_runtime", %error, environment = %environment_id, "registered liveness stamp failed");
                    false
                }
            };
            if restore
                && let Err(error) = EnvironmentStore::mark_registered_connected(
                    store.as_ref(),
                    MarkRegisteredConnected {
                        environment_id: environment_id.clone(),
                        daemon_build,
                        at_ms,
                    },
                )
                .await
            {
                tracing::warn!(target: "temporal_runtime", %error, environment = %environment_id, "restoring a registered connection failed");
            }
        })
    }
}

/// Aborts a background task when the session that owns it ends.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Startup repair, before the environment-gateway listener accepts
/// connections: every registered row that reads connected loses its
/// connection now, since this process held all of them (one
/// environment-gateway replica per deployment). Persistent rows then get
/// their full reconnect wait while their daemons reconnect.
pub async fn repair_registered_connections(
    runtime: &UniverseRuntime,
) -> Result<u64, store_pg::PgStoreError> {
    store_pg::repair_registered_connections(runtime.stores().pool(), now_ms()).await
}

/// The registered sweep of the environment-gateway role, every
/// [`REGISTERED_SWEEP_INTERVAL`]: [`sweep_registered_universe`] for every
/// universe with registered environments, then [`revoke_ended_routes`].
pub async fn run_registered_sweep(runtime: Arc<UniverseRuntime>) {
    let mut interval = tokio::time::interval(REGISTERED_SWEEP_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let universes = match store_pg::list_universes_with_registered_environments(
            runtime.stores().pool(),
        )
        .await
        {
            Ok(universes) => universes,
            Err(error) => {
                tracing::warn!(target: "temporal_runtime", %error, "registered sweep could not list universes");
                Vec::new()
            }
        };
        let now = now_ms();
        for universe_id in universes {
            if let Err(error) = sweep_registered_universe(&runtime, universe_id, now).await {
                tracing::warn!(target: "temporal_runtime", %error, %universe_id, "registered sweep failed");
            }
        }
        if let Err(error) = revoke_ended_routes(&runtime).await {
            tracing::warn!(target: "temporal_runtime", %error, "route revocation sweep failed");
        }
    }
}

/// One universe's registered sweep at `now_ms`: rows whose liveness stamp
/// went stale become disconnected, ephemeral rows away past their key's
/// grace close (`ephemeral_grace`), persistent rows away past the key's
/// `offlineCloseAfterMs` close (`offline_timeout`). Every closed
/// environment's routes are revoked.
pub async fn sweep_registered_universe(
    runtime: &UniverseRuntime,
    universe_id: Uuid,
    now_ms: i64,
) -> Result<RegisteredSweep, EnvironmentRegistryError> {
    let store = runtime.stores().store_for(universe_id);
    let sweep = EnvironmentStore::sweep_registered_environments(store.as_ref(), now_ms).await?;
    for environment_id in &sweep.disconnected {
        tracing::info!(target: "temporal_runtime", %universe_id, environment = %environment_id, "registered environment's liveness stamp went stale; marked disconnected");
    }
    let signals = runtime.environment_signals();
    for (environment_id, reason) in &sweep.closed {
        tracing::info!(target: "temporal_runtime", %universe_id, environment = %environment_id, %reason, "registered sweep closed an environment");
        signals.revoke(universe_id, environment_id.as_str());
    }
    Ok(sweep)
}

/// End the live routes and control connections of environments that are no
/// longer open, or whose provider binding is gone or disabled, when that
/// happened outside this process. Returns how many environments it revoked.
pub async fn revoke_ended_routes(
    runtime: &UniverseRuntime,
) -> Result<usize, store_pg::PgStoreError> {
    let signals = runtime.environment_signals();
    let watched = signals.watched();
    if watched.is_empty() {
        return Ok(0);
    }
    let ended =
        store_pg::list_revoked_environment_routes(runtime.stores().pool(), &watched).await?;
    for (universe_id, environment_id) in &ended {
        signals.revoke(*universe_id, environment_id);
    }
    Ok(ended.len())
}

async fn send_text(socket: &mut WebSocket, text: String) -> Result<(), axum::Error> {
    socket.send(Message::Text(text.into())).await
}

fn new_nonce() -> ChallengeNonce {
    ChallengeNonce::from_bytes(random_bytes())
}

fn new_token() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::TryRng as _;
    let mut bytes = [0u8; N];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random source unavailable");
    bytes
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};
    use environment_protocol::{
        error::{ErrorData, ProtocolErrorKind},
        handshake::ImplementationInfo,
        metadata::{DisplayName, Metadata},
        registration::Ed25519Signature,
        routes::canonical_origin,
    };
    use environments::{
        BeginCloseEnvironment, CreateEnvironmentRegistrationKey, EnvironmentCloseReason,
        EnvironmentRegistrationKeyId, InMemoryEnvironmentRegistryStore, MarkRegisteredDisconnected,
        RegistrationKeyPolicy, RevokeEnvironmentRegistrationKey, mint_registration_key,
    };
    use serde_json::json;

    use super::*;

    fn daemon(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn origin() -> CanonicalOrigin {
        canonical_origin("wss://gateway.example/environment-gateway/connect").unwrap()
    }

    fn register_signed(
        key: &SigningKey,
        nonce: &ChallengeNonce,
        origin: &CanonicalOrigin,
        secret: Option<&str>,
    ) -> RegisterParams {
        RegisterParams {
            protocol_version: ENVD_PROTOCOL_VERSION,
            registration_key: secret.map(SecretString::new),
            daemon_public_key: DaemonPublicKey::from_bytes(key.verifying_key().to_bytes()),
            signature: Ed25519Signature::from_bytes(
                key.sign(&signed_registration_message(nonce, origin))
                    .to_bytes(),
            ),
            display_name: Some(DisplayName::new("worker").unwrap()),
            metadata: Metadata::default(),
            implementation: ImplementationInfo::new("lightspeed-envd")
                .and_then(|info| info.with_version("0.1.0"))
                .and_then(|info| info.with_git_sha("1111111111111111111111111111111111111111"))
                .and_then(|info| info.with_target("x86_64-unknown-linux-musl"))
                .unwrap(),
        }
    }

    fn register(key: &SigningKey, secret: Option<&str>) -> RegisterParams {
        register_signed(key, &ChallengeNonce::from_bytes([1; 32]), &origin(), secret)
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
                secret_hash: minted.secret.secret_hash.clone(),
                record: minted.record.clone(),
            })
            .await
            .expect("create key");
        (store, minted.secret.secret.expose().to_owned())
    }

    fn policy(mode: RegisteredIdentityMode) -> RegistrationKeyPolicy {
        RegistrationKeyPolicy {
            display_name: "pool".to_owned(),
            identity_mode: mode,
            max_active_environments: None,
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: None,
            expires_at_ms: None,
        }
    }

    #[test]
    fn signatures_bind_the_nonce_and_the_gateway_origin() {
        let key = daemon(1);
        let nonce = ChallengeNonce::from_bytes([5; 32]);
        let params = register_signed(&key, &nonce, &origin(), None);
        assert!(verify_register_request(&params, &nonce, &[origin()]).is_ok());

        let replayed =
            verify_register_request(&params, &ChallengeNonce::from_bytes([6; 32]), &[origin()])
                .unwrap_err();
        assert!(matches!(
            replayed.data,
            ErrorData::PermissionDenied {
                reason: Some(PermissionReason::InvalidSignature)
            }
        ));

        // A nonce relayed from another gateway was signed for that origin.
        let elsewhere = canonical_origin("wss://other.example").unwrap();
        let relayed = register_signed(&key, &nonce, &elsewhere, None);
        assert_eq!(
            verify_register_request(&relayed, &nonce, &[origin()])
                .unwrap_err()
                .kind(),
            ProtocolErrorKind::PermissionDenied
        );
        assert!(verify_register_request(&relayed, &nonce, &[origin(), elsewhere]).is_ok());

        let mut wrong_key = params.clone();
        wrong_key.daemon_public_key =
            DaemonPublicKey::from_bytes(daemon(2).verifying_key().to_bytes());
        assert_eq!(
            verify_register_request(&wrong_key, &nonce, &[origin()])
                .unwrap_err()
                .kind(),
            ProtocolErrorKind::PermissionDenied
        );
    }

    fn register_value() -> serde_json::Value {
        serde_json::to_value(register(&daemon(1), None)).unwrap()
    }

    #[test]
    fn register_decoding_checks_the_version_first_and_refuses_out_of_bounds_facts() {
        assert!(decode_register(&register_value()).is_ok());

        // A future daemon with members this gateway does not know still
        // learns the version it must speak.
        let mut future = register_value();
        future["protocolVersion"] = json!(ENVD_PROTOCOL_VERSION + 1);
        future["newMember"] = json!(true);
        let mismatch = decode_register(&future).unwrap_err();
        assert_eq!(
            mismatch.data,
            ErrorData::VersionMismatch {
                expected: ENVD_PROTOCOL_VERSION,
                received: ENVD_PROTOCOL_VERSION + 1,
            }
        );

        let mut poisoned = register_value();
        poisoned["implementation"]["version"] = json!("x".repeat(300));
        assert_eq!(
            decode_register(&poisoned).unwrap_err().kind(),
            ProtocolErrorKind::InvalidRequest
        );
        let mut reserved = register_value();
        reserved["metadata"] = json!({"lightspeed.x": "y"});
        assert_eq!(
            decode_register(&reserved).unwrap_err().kind(),
            ProtocolErrorKind::InvalidRequest
        );

        // Hex that is not canonical, or not ASCII, never panics: it is an
        // ordinary invalidParams before anything else runs.
        for key in [
            "AB".repeat(32),
            format!("{}\u{e9}", "a".repeat(62)),
            "+1".repeat(32),
        ] {
            let mut value = register_value();
            value["daemonPublicKey"] = json!(key);
            assert_eq!(
                decode_register(&value).unwrap_err().kind(),
                ProtocolErrorKind::InvalidParams
            );
        }
        let mut data_url = register_value();
        data_url["dataUrl"] = json!("wss://elsewhere.example/data");
        assert_eq!(
            decode_register(&data_url).unwrap_err().kind(),
            ProtocolErrorKind::InvalidParams
        );
    }

    #[test]
    fn the_failure_limiter_keys_on_identities_and_hints_a_retry() {
        let start = Instant::now();
        let mut limiter = FailureLimiter::default();
        let first = DaemonPublicKey::from_bytes([1; 32]);
        let second = DaemonPublicKey::from_bytes([2; 32]);
        for _ in 0..MAX_FAILURES_PER_WINDOW {
            assert_eq!(limiter.retry_after(&first, start), None);
            limiter.record(first, start);
        }
        let hint = limiter.retry_after(&first, start).expect("over the limit");
        assert!(hint <= FAILURE_WINDOW && hint > Duration::ZERO);
        assert_eq!(limiter.retry_after(&second, start), None);
        assert_eq!(
            limiter.retry_after(&first, start + FAILURE_WINDOW),
            None,
            "the window slides"
        );

        // Expired entries are pruned on the next insert.
        limiter.record(second, start + FAILURE_WINDOW);
        assert_eq!(limiter.len(), 1);
    }

    #[test]
    fn the_failure_limiter_is_capped() {
        let now = Instant::now();
        let mut limiter = FailureLimiter::default();
        for index in 0..=MAX_LIMITED_IDENTITIES {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
            limiter.record(
                DaemonPublicKey::from_bytes(bytes),
                now + Duration::from_millis(index as u64),
            );
        }
        assert_eq!(limiter.len(), MAX_LIMITED_IDENTITIES);
        assert!(
            !limiter
                .identities
                .contains_key(&DaemonPublicKey::from_bytes([0; 32])),
            "the identity whose last failure is oldest is forgotten first"
        );
    }

    #[test]
    fn only_credential_refusals_count_against_an_identity() {
        assert!(counts_as_failure(&unknown_registration_key()));
        assert!(counts_as_failure(&unknown_daemon()));
        assert!(counts_as_failure(&ProtocolError::expired("x")));
        assert!(!counts_as_failure(&registration_unavailable()));
        assert!(!counts_as_failure(&ProtocolError::busy(
            Some(BusyReason::CapacityExhausted),
            None,
            "x"
        )));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_creates_once_reconnects_by_identity_and_spends_closed_identities() {
        let (store, secret) = store_with_key("rk", policy(RegisteredIdentityMode::Ephemeral)).await;
        let key = daemon(3);
        let unknown = admit_registration(&store, &register(&key, None), 2_000)
            .await
            .unwrap_err();
        assert!(matches!(
            unknown.data,
            ErrorData::NotFound {
                reason: Some(NotFoundReason::UnknownDaemon)
            }
        ));

        let created = admit_registration(&store, &register(&key, Some(&secret)), 2_000)
            .await
            .expect("created");
        assert_eq!(created.admission, Admission::Created);
        assert_eq!(created.environment.state, EnvironmentState::Open);
        assert_eq!(created.environment.request_id, None);
        let source = created.environment.registered().expect("registered");
        assert!(source.connected);
        assert_eq!(source.connection_changed_at_ms, 2_000);
        assert_eq!(source.last_seen_at_ms, 2_000);
        let build = created.environment.daemon_build.as_ref().expect("build");
        assert_eq!(build.version.as_deref(), Some("0.1.0"));
        assert_eq!(build.protocol_version, ENVD_PROTOCOL_VERSION);
        assert!(
            created.environment.metadata.is_empty(),
            "build facts are columns, not metadata"
        );

        let connection = ControlConnectionId::new("connection_1").unwrap();
        let receipt =
            registration_receipt(&created.environment, &connection, created.admission).unwrap();
        assert_eq!(receipt.identity_mode, RegistrationIdentityMode::Ephemeral);
        assert_eq!(receipt.admission, Admission::Created);
        assert_eq!(receipt.control_connection_id, connection);
        assert_eq!(
            receipt.environment_id.as_str(),
            created.environment.environment_id.as_str()
        );
        assert_eq!(receipt.daemon_id.as_str(), source.daemon_id().as_str());
        assert!(receipt.daemon_id.as_str().starts_with("daemon_"));

        // A dropped connection, then a reconnect that ignores the key
        // entirely, even a wrong one, and records the connection again.
        store
            .mark_registered_disconnected(MarkRegisteredDisconnected {
                environment_id: created.environment.environment_id.clone(),
                at_ms: 2_500,
            })
            .await
            .expect("disconnect");
        let plan = plan_admission(&store, &register(&key, Some("lsrk_wrong")), 3_000)
            .await
            .expect("plan");
        assert!(matches!(plan, AdmissionPlan::Reconnect { .. }));
        assert_eq!(plan.environment_id(), &created.environment.environment_id);
        let reconnected = commit_admission(&store, plan, 3_000)
            .await
            .expect("reconnect");
        assert_eq!(reconnected.admission, Admission::Reconnected);
        assert_eq!(
            reconnected.environment.environment_id,
            created.environment.environment_id
        );
        let source = reconnected.environment.registered().expect("registered");
        assert!(source.connected);
        assert_eq!(source.connection_changed_at_ms, 3_000);

        // Closing a registered environment is immediate and spends the
        // identity for good.
        store
            .begin_close_environment(BeginCloseEnvironment {
                environment_id: created.environment.environment_id.clone(),
                reason: EnvironmentCloseReason::Requested,
                at_ms: 4_000,
            })
            .await
            .expect("close");
        let closed = admit_registration(&store, &register(&key, Some(&secret)), 4_100)
            .await
            .unwrap_err();
        assert!(matches!(
            closed.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::EnvironmentClosed
            }
        ));
        assert!(!closed.is_retryable());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_reconnect_planned_before_a_close_is_refused_as_closed() {
        let (store, secret) =
            store_with_key("rk", policy(RegisteredIdentityMode::Persistent)).await;
        let key = daemon(9);
        let created = admit_registration(&store, &register(&key, Some(&secret)), 2_000)
            .await
            .expect("created");
        let plan = plan_admission(&store, &register(&key, None), 3_000)
            .await
            .expect("plan");
        store
            .begin_close_environment(BeginCloseEnvironment {
                environment_id: created.environment.environment_id.clone(),
                reason: EnvironmentCloseReason::Requested,
                at_ms: 3_100,
            })
            .await
            .expect("close");
        let refused = commit_admission(&store, plan, 3_200).await.unwrap_err();
        assert!(matches!(
            refused.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::EnvironmentClosed
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn only_an_ephemeral_departure_closes_its_environment() {
        let (ephemeral_store, ephemeral_secret) =
            store_with_key("rk-e", policy(RegisteredIdentityMode::Ephemeral)).await;
        let ephemeral = admit_registration(
            &ephemeral_store,
            &register(&daemon(10), Some(&ephemeral_secret)),
            1_000,
        )
        .await
        .expect("ephemeral");
        assert!(departure_closes(&ephemeral.environment));
        let (persistent_store, persistent_secret) =
            store_with_key("rk-p", policy(RegisteredIdentityMode::Persistent)).await;
        let persistent = admit_registration(
            &persistent_store,
            &register(&daemon(11), Some(&persistent_secret)),
            1_000,
        )
        .await
        .expect("persistent");
        assert!(!departure_closes(&persistent.environment));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connection_transitions_of_one_environment_run_one_at_a_time() {
        let registrations = Arc::new(RegisteredConnections::new());
        let universe_id = Uuid::nil();
        let held = registrations.transition(universe_id, "env").await;
        // Another environment's transition is independent.
        drop(registrations.transition(universe_id, "other").await);
        let waiting = {
            let registrations = registrations.clone();
            tokio::spawn(async move {
                drop(registrations.transition(universe_id, "env").await);
            })
        };
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "a second transition waits for the first"
        );
        drop(held);
        waiting.await.unwrap();
        assert!(
            registrations.transitions.lock().unwrap().is_empty(),
            "finished transitions leave no lock behind"
        );
    }

    #[test]
    fn a_failed_admission_hands_the_environment_back_to_the_displaced_connection() {
        let registrations = RegisteredConnections::new();
        let universe_id = Uuid::nil();
        let handle = |id: &str| {
            let (commands, _) = mpsc::channel(1);
            let (supersede, _) = oneshot::channel();
            let (ended, _) = watch::channel(());
            ControlHandle {
                connection_id: ControlConnectionId::new(id).unwrap(),
                commands,
                supersede,
                ended,
            }
        };
        let old = ControlConnectionId::new("connection_old").unwrap();
        let new = ControlConnectionId::new("connection_new").unwrap();
        assert!(
            registrations
                .install(universe_id, "env".to_owned(), handle("connection_old"))
                .is_none()
        );
        let previous =
            registrations.install(universe_id, "env".to_owned(), handle("connection_new"));
        let ended = previous.as_ref().expect("displaced").ended.subscribe();
        assert_eq!(registrations.current(universe_id, "env"), Some(new.clone()));
        assert!(registrations.uninstall(universe_id, "env", &new, previous));
        assert_eq!(registrations.current(universe_id, "env"), Some(old.clone()));
        assert!(
            ended.has_changed().is_ok(),
            "the restored handle keeps its routes"
        );
        assert!(
            !registrations.remove_if_current(universe_id, "env", &new),
            "a connection that is no longer current removes nothing"
        );
        assert!(registrations.remove_if_current(universe_id, "env", &old));
        assert_eq!(registrations.current(universe_id, "env"), None);
        assert!(
            ended.has_changed().is_err(),
            "dropping a handle ends the routes paired under it"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_maps_key_policy_refusals_to_typed_kinds() {
        let (store, secret) = store_with_key(
            "rk",
            RegistrationKeyPolicy {
                max_active_environments: Some(1),
                ..policy(RegisteredIdentityMode::Persistent)
            },
        )
        .await;
        admit_registration(&store, &register(&daemon(4), Some(&secret)), 2_000)
            .await
            .expect("first");
        let full = admit_registration(&store, &register(&daemon(5), Some(&secret)), 2_100)
            .await
            .unwrap_err();
        assert!(matches!(
            full.data,
            ErrorData::Busy {
                reason: Some(BusyReason::CapacityExhausted),
                ..
            }
        ));
        assert!(full.is_retryable());

        let bad = admit_registration(&store, &register(&daemon(6), Some("lsrk_nope")), 2_200)
            .await
            .unwrap_err();
        assert!(matches!(
            bad.data,
            ErrorData::PermissionDenied {
                reason: Some(PermissionReason::InvalidRegistrationKey)
            }
        ));

        store
            .revoke_registration_key(RevokeEnvironmentRegistrationKey {
                registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
                revoked_at_ms: 3_000,
            })
            .await
            .expect("revoke");
        let revoked = admit_registration(&store, &register(&daemon(7), Some(&secret)), 3_100)
            .await
            .unwrap_err();
        assert!(matches!(
            revoked.data,
            ErrorData::PermissionDenied {
                reason: Some(PermissionReason::RegistrationKeyRevoked)
            }
        ));
        // The already registered daemon still reconnects after revocation.
        let reconnect = admit_registration(&store, &register(&daemon(4), None), 3_200)
            .await
            .expect("reconnect");
        assert_eq!(reconnect.admission, Admission::Reconnected);

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
            &register(&daemon(8), Some(&expired_secret)),
            2_000,
        )
        .await
        .unwrap_err();
        assert_eq!(expired.kind(), ProtocolErrorKind::Expired);
    }

    fn text(value: serde_json::Value) -> Message {
        Message::Text(value.to_string().into())
    }

    #[test]
    fn before_admission_only_register_is_served() {
        let register = text(json!({
            "jsonrpc": "2.0", "id": 1, "method": "registration/register", "params": {}
        }));
        assert!(matches!(
            classify_pre_admission(&register),
            PreAdmission::Register(_)
        ));
        let other = text(json!({"jsonrpc": "2.0", "id": 2, "method": "env/getIdle"}));
        let PreAdmission::AnswerAndClose(response) = classify_pre_admission(&other) else {
            panic!("a request other than register is refused and closes");
        };
        assert_eq!(
            response.result.unwrap_err().kind(),
            ProtocolErrorKind::NotInitialized
        );
        let goodbye = text(json!({
            "jsonrpc": "2.0", "method": "registration/goodbye", "params": {"reason": "shutdown"}
        }));
        assert_eq!(classify_pre_admission(&goodbye), PreAdmission::Close);
        let PreAdmission::Answer(response) =
            classify_pre_admission(&Message::Text("not json".into()))
        else {
            panic!("a parse error is answered and the socket stays open");
        };
        assert_eq!(response.id, None);
        assert_eq!(
            response.result.unwrap_err().kind(),
            ProtocolErrorKind::ParseError
        );
        assert!(matches!(
            classify_pre_admission(&Message::Binary(vec![1_u8].into())),
            PreAdmission::Answer(_)
        ));
        let stray = text(json!({"jsonrpc": "2.0", "id": "g9", "result": {}}));
        assert_eq!(classify_pre_admission(&stray), PreAdmission::Ignore);
    }

    #[test]
    fn after_admission_stream_answers_and_goodbye_are_recognized() {
        assert_eq!(
            classify_admitted(&json!({"jsonrpc": "2.0", "id": "g1", "result": {}}).to_string()),
            Admitted::StreamAnswer(Id::String("g1".to_owned()), Ok(()))
        );
        let Admitted::StreamAnswer(_, Err(error)) = classify_admitted(
            &jsonrpc::Response::failure(
                Some(Id::String("g2".to_owned())),
                ProtocolError::unavailable("dial failed"),
            )
            .to_text(),
        ) else {
            panic!("a failed dial is an error answer");
        };
        assert_eq!(error.kind(), ProtocolErrorKind::Unavailable);
        assert_eq!(
            classify_admitted(
                &json!({
                    "jsonrpc": "2.0", "method": "registration/goodbye",
                    "params": {"reason": "shutdown"}
                })
                .to_string()
            ),
            Admitted::Goodbye
        );
        let Admitted::Answer(again) = classify_admitted(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "registration/register"}).to_string(),
        ) else {
            panic!("a second register is answered");
        };
        assert_eq!(
            again.result.unwrap_err().kind(),
            ProtocolErrorKind::InvalidRequest
        );
        let Admitted::Answer(unknown) = classify_admitted(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "registration/unknown"}).to_string(),
        ) else {
            panic!("an unknown request is answered");
        };
        assert_eq!(
            unknown.result.unwrap_err().kind(),
            ProtocolErrorKind::MethodNotFound
        );
        assert!(matches!(
            classify_admitted(&json!({"jsonrpc": "2.0", "method": "x/y"}).to_string()),
            Admitted::Ignore(_)
        ));
    }

    #[test]
    fn the_liveness_deadline_is_three_heartbeats() {
        assert_eq!(
            LIVENESS_TIMEOUT,
            HEARTBEAT_INTERVAL * HEARTBEAT_MISSED_INTERVALS
        );
    }
}
