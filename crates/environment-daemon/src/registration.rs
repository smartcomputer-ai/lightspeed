//! Outbound registration: dial the environment gateway, prove the daemon
//! identity, and serve reverse-dialed streams for as long as the control
//! connection lives.
//!
//! The control connection runs the shared JSON-RPC envelope with the daemon
//! as the client (the frame rules and liveness rule are in
//! `environment_protocol::registration`). The daemon reconnects with
//! backoff after disconnects, heartbeat timeouts and retryable refusals,
//! and ends the process on everything else, each with its own exit status
//! ([`DaemonExit`]): a deliberate shutdown says goodbye first, a terminal
//! refusal or an unsupported gateway protocol is `Refused`, and a
//! `registration/closed` notification ends it as `EnvironmentClosed` or
//! `Superseded`. `EnvironmentClosed`, from that notification or an
//! `environmentClosed` admission refusal, ends only this loop: the server
//! exits or parks, as `--on-closed` says.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    bytes::SecretString,
    error::{ErrorData, PreconditionReason, ProtocolError},
    jsonrpc::{self, Id, Message as RpcMessage, Notification, Request, Response, ResponseError},
    limits::HEARTBEAT_MISSED_INTERVALS,
    manifest::{
        RegistrationClosed, RegistrationGoodbye, RegistrationOpenStream, RegistrationRegister,
    },
    registration::{
        ChallengeParams, ClosedParams, ClosedReason, GoodbyeParams, GoodbyeReason,
        OpenStreamParams, OpenStreamResponse, RegisterParams, RegisterResponse,
        RegistrationReceipt, StreamSpec, challenge_protocol_version,
    },
};
use futures_util::{SinkExt as _, StreamExt as _, stream::SplitSink, stream::SplitStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest as _,
        http::{HeaderValue, header::AUTHORIZATION, header::USER_AGENT},
    },
};

use crate::{
    DaemonRuntime,
    config::{
        AUTO_UPGRADE_ATTEMPTED_ENV, GatewayUrls, REEXEC_REGISTRATION_KEY_ENV, RegistrationConfig,
        RestartInvocation,
    },
    identity::DaemonIdentity,
    server::{DaemonExit, run_data_connection, tracing_line, websocket_config},
};

/// Bound on TCP connect, TLS and the WebSocket upgrade of the control
/// connection.
const DIAL_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the daemon waits for the gateway's first frame, and then for
/// the register response.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on dialing one reverse stream, upgrade included.
const STREAM_DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a shutting-down daemon spends on goodbye and the close frame.
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(2);
/// Shortest heartbeat interval the liveness deadline is derived from.
const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A control session that lasted this long resets the reconnect backoff.
const STABLE_SESSION: Duration = Duration::from_secs(30);
/// The id of the daemon's only request on a control connection.
const REGISTER_ID: i64 = 1;

type ControlSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type ControlWriter = SplitSink<ControlSocket, Message>;
type ControlReader = SplitStream<ControlSocket>;

/// Why one control session ended.
#[derive(Debug)]
enum SessionEnd {
    /// The gateway or network went away, the liveness deadline passed, or
    /// the gateway broke the frame rules; reconnect with backoff.
    Disconnected(String),
    /// The gateway refused admission for a transient reason; reconnect no
    /// sooner than its hint.
    Retry {
        reason: String,
        retry_after: Option<Duration>,
    },
    /// The gateway speaks this other protocol version.
    ProtocolMismatch(u32),
    /// The gateway's first frame is unreadable: an incompatible gateway
    /// whose protocol only its discovery document can tell.
    IncompatibleGateway(String),
    /// End the process.
    Exit(DaemonExit),
}

/// Register with the configured gateway and serve it until the process
/// must end.
pub async fn run_outbound(runtime: DaemonRuntime, registration: RegistrationConfig) -> DaemonExit {
    let outbound = match Outbound::new(runtime, registration) {
        Ok(outbound) => outbound,
        Err(error) => return DaemonExit::Configuration(error),
    };
    outbound.run().await
}

struct Outbound {
    runtime: DaemonRuntime,
    registration: RegistrationConfig,
    urls: GatewayUrls,
    identity: DaemonIdentity,
    tls: TlsSettings,
    /// Held only until the first admission.
    registration_key: Option<SecretString>,
}

impl Outbound {
    fn new(runtime: DaemonRuntime, registration: RegistrationConfig) -> Result<Self> {
        let urls = registration.validate()?;
        if let Some(path) = &registration.receipt_path {
            check_receipt_path(path)?;
        }
        let identity = DaemonIdentity::load_or_create(&runtime.config().state_dir)
            .context("load daemon identity")?;
        let tls = TlsSettings::from_config(registration.ca_file.as_deref())?;
        let registration_key = registration.registration_key.clone();
        Ok(Self {
            runtime,
            registration,
            urls,
            identity,
            tls,
            registration_key,
        })
    }

    async fn run(mut self) -> DaemonExit {
        tracing_line(&format!(
            "registering with {} as daemon public key {}",
            self.urls.connect,
            self.identity.public_key()
        ));
        let shutdown = self.runtime.shutdown().clone();
        let mut backoff = INITIAL_BACKOFF;
        loop {
            let started = Instant::now();
            let mut retry_after = None;
            match self.session().await {
                SessionEnd::Exit(exit) => return exit,
                SessionEnd::Disconnected(reason) => {
                    tracing_line(&format!("control connection ended: {reason}"));
                }
                SessionEnd::Retry {
                    reason,
                    retry_after: hint,
                } => {
                    tracing_line(&format!("registration deferred: {reason}"));
                    retry_after = hint;
                }
                SessionEnd::ProtocolMismatch(gateway_protocol) => {
                    match self.upgrade(Some(gateway_protocol), None).await {
                        SessionEnd::Exit(exit) => return exit,
                        SessionEnd::Retry { reason, .. } | SessionEnd::Disconnected(reason) => {
                            tracing_line(&reason);
                        }
                        SessionEnd::ProtocolMismatch(_) | SessionEnd::IncompatibleGateway(_) => {}
                    }
                }
                SessionEnd::IncompatibleGateway(reason) => {
                    match self.upgrade(None, Some(reason)).await {
                        SessionEnd::Exit(exit) => return exit,
                        SessionEnd::Retry { reason, .. } | SessionEnd::Disconnected(reason) => {
                            tracing_line(&reason);
                        }
                        SessionEnd::ProtocolMismatch(_) | SessionEnd::IncompatibleGateway(_) => {}
                    }
                }
            }
            if started.elapsed() >= STABLE_SESSION {
                backoff = INITIAL_BACKOFF;
            }
            let delay = jittered(backoff).max(retry_after.unwrap_or_default());
            tokio::select! {
                _ = shutdown.cancelled() => return DaemonExit::Shutdown,
                _ = tokio::time::sleep(delay) => {}
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// One control connection, from dial to its end.
    async fn session(&mut self) -> SessionEnd {
        let shutdown = self.runtime.shutdown().clone();
        let dialed = tokio::select! {
            _ = shutdown.cancelled() => return SessionEnd::Exit(DaemonExit::Shutdown),
            dialed = tokio::time::timeout(
                DIAL_TIMEOUT,
                dial(&self.urls.connect, None, &self.tls),
            ) => dialed,
        };
        let socket = match dialed {
            Ok(Ok(socket)) => socket,
            Ok(Err(error)) => return SessionEnd::Disconnected(format!("{error:#}")),
            Err(_) => {
                return SessionEnd::Disconnected(format!(
                    "connecting to {} timed out after {} s",
                    self.urls.connect,
                    DIAL_TIMEOUT.as_secs()
                ));
            }
        };
        let (mut writer, mut reader) = socket.split();
        let end = self.admitted_session(&mut writer, &mut reader).await;
        if !matches!(end, SessionEnd::Exit(DaemonExit::Shutdown)) {
            let _ = tokio::time::timeout(GOODBYE_TIMEOUT, writer.close()).await;
        }
        end
    }

    async fn admitted_session(
        &mut self,
        writer: &mut ControlWriter,
        reader: &mut ControlReader,
    ) -> SessionEnd {
        let shutdown = self.runtime.shutdown().clone();

        // 1. The frozen first frame: its version is read before anything
        //    else, so a gateway of any version can tell this daemon it is
        //    stale.
        let first = tokio::select! {
            _ = shutdown.cancelled() => return SessionEnd::Exit(DaemonExit::Shutdown),
            first = tokio::time::timeout(HANDSHAKE_TIMEOUT, next_text(reader)) => first,
        };
        let first = match first {
            Ok(Ok(text)) => text,
            Ok(Err(end)) => return end,
            Err(_) => return SessionEnd::Disconnected("no challenge from the gateway".to_owned()),
        };
        let gateway_protocol = match challenge_protocol_version(&first) {
            Ok(version) => version,
            Err(error) => {
                return SessionEnd::IncompatibleGateway(format!(
                    "{} sent an unreadable first frame ({error})",
                    self.urls.connect
                ));
            }
        };
        if gateway_protocol != ENVD_PROTOCOL_VERSION {
            return SessionEnd::ProtocolMismatch(gateway_protocol);
        }
        let challenge = match decode_challenge(&first) {
            Ok(challenge) => challenge,
            Err(error) => return SessionEnd::Disconnected(format!("{error:#}")),
        };

        // 2. Register, signing the nonce bound to the dialed origin.
        let params = RegisterParams {
            protocol_version: ENVD_PROTOCOL_VERSION,
            registration_key: self.registration_key.clone(),
            daemon_public_key: self.identity.public_key(),
            signature: self
                .identity
                .sign_challenge(&challenge.nonce, &self.urls.origin),
            display_name: self.registration.display_name.clone(),
            metadata: self.registration.metadata.clone(),
            implementation: self.runtime.server_info().clone(),
        };
        let request =
            match Request::new(Id::Number(REGISTER_ID), RegistrationRegister::NAME, &params) {
                Ok(request) => request,
                Err(error) => {
                    return SessionEnd::Exit(DaemonExit::Failed(anyhow!(
                        "encode register: {error}"
                    )));
                }
            };
        if let Err(error) = writer.send(Message::Text(request.to_text().into())).await {
            return SessionEnd::Disconnected(format!("send register: {error}"));
        }
        let response = tokio::select! {
            _ = shutdown.cancelled() => return SessionEnd::Exit(DaemonExit::Shutdown),
            response = tokio::time::timeout(HANDSHAKE_TIMEOUT, register_response(reader)) => response,
        };
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(end)) => return end,
            Err(_) => {
                return SessionEnd::Disconnected(
                    "no register response from the gateway".to_owned(),
                );
            }
        };
        let admitted = match response.decode::<RegisterResponse>() {
            Ok(admitted) => admitted,
            Err(ResponseError::Failed(error)) => return refusal(error),
            Err(ResponseError::Undecodable(message)) => {
                return SessionEnd::Disconnected(format!(
                    "the register response breaks the contract: {message}"
                ));
            }
        };

        // 3. Admitted: the key has done its job.
        self.registration_key = None;
        if let Err(error) =
            announce_receipt(&admitted.receipt, self.registration.receipt_path.as_deref())
        {
            return SessionEnd::Exit(DaemonExit::Configuration(error));
        }
        let interval =
            Duration::from_millis(admitted.heartbeat_interval_ms).max(MIN_HEARTBEAT_INTERVAL);
        let liveness = interval * HEARTBEAT_MISSED_INTERVALS;
        self.serve_admitted(writer, reader, liveness).await
    }

    /// The admitted control connection: answer `registration/openStream`,
    /// obey `registration/closed`, and treat `liveness` without any frame
    /// as a dead connection.
    async fn serve_admitted(
        &self,
        writer: &mut ControlWriter,
        reader: &mut ControlReader,
        liveness: Duration,
    ) -> SessionEnd {
        let shutdown = self.runtime.shutdown().clone();
        let (answers, mut queued) = mpsc::channel::<String>(64);
        let mut deadline = tokio::time::Instant::now() + liveness;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    say_goodbye(writer).await;
                    return SessionEnd::Exit(DaemonExit::Shutdown);
                }
                Some(text) = queued.recv() => {
                    if let Err(error) = writer.send(Message::Text(text.into())).await {
                        return SessionEnd::Disconnected(format!("send to the gateway: {error}"));
                    }
                }
                incoming = tokio::time::timeout_at(deadline, reader.next()) => {
                    let Ok(incoming) = incoming else {
                        return SessionEnd::Disconnected(format!(
                            "no frame from the gateway for {} ms (heartbeat timeout)",
                            liveness.as_millis()
                        ));
                    };
                    deadline = tokio::time::Instant::now() + liveness;
                    let message = match incoming {
                        Some(Ok(message)) => message,
                        Some(Err(error)) => return SessionEnd::Disconnected(error.to_string()),
                        None => return SessionEnd::Disconnected("gateway closed".to_owned()),
                    };
                    match message {
                        Message::Text(text) => {
                            if let Some(end) = self.on_admitted_text(text.as_str(), &answers) {
                                return end;
                            }
                        }
                        Message::Binary(_) => {
                            return SessionEnd::Disconnected(
                                "the gateway sent a binary frame".to_owned(),
                            );
                        }
                        Message::Close(frame) => {
                            return SessionEnd::Disconnected(
                                frame
                                    .map(|frame| frame.reason.to_string())
                                    .filter(|reason| !reason.is_empty())
                                    .unwrap_or_else(|| "gateway closed".to_owned()),
                            );
                        }
                        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
            }
        }
    }

    /// Handle one text frame after admission; `Some` ends the session.
    fn on_admitted_text(&self, text: &str, answers: &mpsc::Sender<String>) -> Option<SessionEnd> {
        match jsonrpc::parse(text) {
            Err(rejection) => Some(SessionEnd::Disconnected(format!(
                "the gateway sent an invalid frame: {}",
                rejection.error
            ))),
            Ok(RpcMessage::Request(request)) => {
                if request.method == RegistrationOpenStream::NAME {
                    match request.params::<OpenStreamParams>() {
                        Ok(params) => self.open_stream(request.id, params, answers.clone()),
                        Err(error) => answer(answers, Response::failure(Some(request.id), error)),
                    }
                } else {
                    answer(
                        answers,
                        Response::failure(
                            Some(request.id),
                            ProtocolError::method_not_found(&request.method),
                        ),
                    );
                }
                None
            }
            Ok(RpcMessage::Notification(notification)) => self.on_notification(notification),
            Ok(RpcMessage::Response(response)) => {
                tracing_line(&format!(
                    "ignoring a gateway response to id {}",
                    response
                        .id
                        .map_or_else(|| "null".to_owned(), |id| id.to_string())
                ));
                None
            }
        }
    }

    fn on_notification(&self, notification: Notification) -> Option<SessionEnd> {
        if notification.method != RegistrationClosed::NAME {
            tracing_line(&format!(
                "ignoring gateway notification {}",
                notification.method
            ));
            return None;
        }
        let params = match notification.params::<ClosedParams>() {
            Ok(params) => params,
            Err(error) => {
                tracing_line(&format!(
                    "ignoring undecodable registration/closed: {error}"
                ));
                return None;
            }
        };
        if let Some(message) = &params.message {
            tracing_line(&format!("gateway closed the session: {}", message.as_str()));
        }
        Some(SessionEnd::Exit(match params.reason {
            ClosedReason::Superseded => DaemonExit::Superseded {
                daemon_public_key: self.identity.public_key().to_hex(),
            },
            ClosedReason::EnvironmentClosed => DaemonExit::EnvironmentClosed,
        }))
    }

    /// Dial the stream the gateway asked for on its own task and answer
    /// once it is upgraded (or failed), then serve it.
    fn open_stream(&self, id: Id, params: OpenStreamParams, answers: mpsc::Sender<String>) {
        let OpenStreamParams { token, stream } = params;
        let StreamSpec::DataProtocol {} = stream;
        let runtime = self.runtime.clone();
        let stream_url = self.urls.stream.clone();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            let dialed =
                tokio::time::timeout(STREAM_DIAL_TIMEOUT, dial(&stream_url, Some(&token), &tls))
                    .await;
            let socket = match dialed {
                Ok(Ok(socket)) => socket,
                Ok(Err(error)) => {
                    tracing_line(&format!("stream dial failed: {error:#}"));
                    let error = ProtocolError::unavailable(format!("dial {stream_url}: {error:#}"));
                    let _ = answers
                        .send(Response::failure(Some(id), error).to_text())
                        .await;
                    return;
                }
                Err(_) => {
                    let error = ProtocolError::unavailable(format!(
                        "dialing {stream_url} timed out after {} s",
                        STREAM_DIAL_TIMEOUT.as_secs()
                    ));
                    let _ = answers
                        .send(Response::failure(Some(id), error).to_text())
                        .await;
                    return;
                }
            };
            let _ = answers
                .send(Response::success(id, &OpenStreamResponse {}).to_text())
                .await;
            drop(answers);
            run_data_connection(&runtime, socket).await;
        });
    }

    /// Install the gateway's build and re-execute it, or say why not.
    /// Install the gateway's envd build and re-execute it: the build of
    /// `gateway_protocol` when the challenge named one, otherwise (an
    /// unreadable first frame, explained by `unreadable`) whatever protocol
    /// discovery publishes, provided it is not this daemon's own. Each
    /// lineage tries once per target protocol, and once after an
    /// unreadable frame.
    async fn upgrade(
        &self,
        gateway_protocol: Option<u32>,
        unreadable: Option<String>,
    ) -> SessionEnd {
        let base = match (gateway_protocol, &unreadable) {
            (Some(gateway_protocol), _) => mismatch_summary(&self.registration, gateway_protocol),
            (None, reason) => format!(
                "{}; it is not an environment gateway this daemon can register with; discovery document: {}",
                reason.as_deref().unwrap_or("the gateway is incompatible"),
                self.registration.discovery_url
            ),
        };
        if let Some(refusal) = upgrade_refusal(&self.registration, gateway_protocol) {
            return SessionEnd::Exit(DaemonExit::Refused(format!("{base}; {refusal}")));
        }
        let Some(restart) = &self.registration.auto_upgrade else {
            return SessionEnd::Exit(DaemonExit::Refused(base));
        };
        tracing_line(&format!(
            "{base}; automatic upgrade enabled, installing the gateway's envd build"
        ));
        let request = crate::upgrade::UpgradeRequest {
            discovery_url: self.registration.discovery_url.clone(),
            ca_file: self.registration.ca_file.clone(),
            install_path: restart.executable.clone(),
            target: release_info::TARGET.to_owned(),
            expected_protocol: gateway_protocol,
            other_than: gateway_protocol.is_none().then_some(ENVD_PROTOCOL_VERSION),
        };
        let installed = tokio::select! {
            _ = self.runtime.shutdown().cancelled() => {
                return SessionEnd::Exit(DaemonExit::Shutdown);
            }
            installed = crate::upgrade::install(request) => installed,
        };
        let installed = match installed {
            Ok(installed) => installed,
            Err(error) if error.is_transient() => {
                return SessionEnd::Retry {
                    reason: format!("{base}; automatic upgrade will retry: {error}"),
                    retry_after: None,
                };
            }
            Err(error) => {
                return SessionEnd::Exit(DaemonExit::Refused(format!(
                    "{base}; automatic upgrade failed: {error}"
                )));
            }
        };
        tracing_line(&format!(
            "installed lightspeed-envd {} ({}, protocol {}); re-executing {}",
            installed.version,
            installed.git_sha,
            installed.protocol_version,
            restart.executable.display()
        ));
        let error = exec_replacement(
            restart,
            installed.protocol_version,
            self.registration_key.as_ref(),
        );
        SessionEnd::Exit(DaemonExit::Refused(format!(
            "{base}; installed the replacement but could not re-execute it: {error}"
        )))
    }
}

/// The challenge params, decoded tolerantly: the frame is frozen and may
/// carry members this daemon does not know.
fn decode_challenge(frame: &str) -> Result<ChallengeParams> {
    let value: serde_json::Value = serde_json::from_str(frame).context("decode challenge")?;
    let params = value
        .get("params")
        .cloned()
        .ok_or_else(|| anyhow!("challenge has no params"))?;
    serde_json::from_value(params).context("decode challenge params")
}

/// Map a register error to what the daemon does next.
fn refusal(error: ProtocolError) -> SessionEnd {
    match &error.data {
        ErrorData::VersionMismatch { expected, .. } => SessionEnd::ProtocolMismatch(*expected),
        ErrorData::FailedPrecondition {
            reason: PreconditionReason::EnvironmentClosed,
        } => SessionEnd::Exit(DaemonExit::EnvironmentClosed),
        ErrorData::Busy { retry_after_ms, .. } | ErrorData::Unavailable { retry_after_ms } => {
            SessionEnd::Retry {
                reason: error.to_string(),
                retry_after: retry_after_ms.map(Duration::from_millis),
            }
        }
        _ if error.is_retryable() => SessionEnd::Retry {
            reason: error.to_string(),
            retry_after: None,
        },
        _ => SessionEnd::Exit(DaemonExit::Refused(error.to_string())),
    }
}

fn mismatch_summary(registration: &RegistrationConfig, gateway_protocol: u32) -> String {
    format!(
        "gateway speaks protocol {gateway_protocol}; this daemon speaks {ENVD_PROTOCOL_VERSION}; discovery document: {}",
        registration.discovery_url
    )
}

/// Why no automatic upgrade runs for this gateway protocol (`None`: an
/// unreadable first frame), if it does not.
fn upgrade_refusal(
    registration: &RegistrationConfig,
    gateway_protocol: Option<u32>,
) -> Option<String> {
    if registration.auto_upgrade.is_none() {
        return Some(
            "run `lightspeed-envd upgrade` or set LIGHTSPEED_ENVD_AUTO_UPGRADE=1".to_owned(),
        );
    }
    match (gateway_protocol, registration.upgrade_attempted_for) {
        (Some(gateway_protocol), Some(attempted)) if attempted == gateway_protocol => {
            Some(format!(
                "an automatic upgrade to protocol {gateway_protocol} already ran in this process lineage and the result still does not match"
            ))
        }
        (None, Some(attempted)) => Some(format!(
            "an automatic upgrade (to protocol {attempted}) already ran in this process lineage and the gateway's first frame is still unreadable"
        )),
        _ => None,
    }
}

/// Arguments of the restarted daemon: the original ones without any
/// registration key option, which travels only through the one-exec
/// variable and only while it is still needed.
fn replacement_args(args: &[std::ffi::OsString]) -> Vec<std::ffi::OsString> {
    let mut kept = Vec::with_capacity(args.len());
    let mut skip_value = false;
    for arg in args {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        let text = arg.to_string_lossy();
        if text == "--registration-key" || text == "--registration-key-file" {
            skip_value = true;
            continue;
        }
        if text.starts_with("--registration-key=") || text.starts_with("--registration-key-file=") {
            continue;
        }
        kept.push(arg.clone());
    }
    kept
}

#[cfg(unix)]
fn exec_replacement(
    restart: &RestartInvocation,
    gateway_protocol: u32,
    registration_key: Option<&SecretString>,
) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;

    let mut command = std::process::Command::new(&restart.executable);
    command
        .arg0(&restart.arg0)
        .args(replacement_args(&restart.args))
        .env(AUTO_UPGRADE_ATTEMPTED_ENV, gateway_protocol.to_string())
        .env_remove("LIGHTSPEED_ENVD_REGISTRATION_KEY")
        .env_remove("LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE")
        .env_remove(REEXEC_REGISTRATION_KEY_ENV);
    // Configuration removed the key variables from this process. A daemon
    // not yet admitted hands the held key across exactly this exec, so it
    // can still register even if its key file was removed; an admitted one
    // proves itself with its private key alone.
    if let Some(registration_key) = registration_key {
        command.env(REEXEC_REGISTRATION_KEY_ENV, registration_key.expose());
    }
    command.exec()
}

#[cfg(not(unix))]
fn exec_replacement(
    _restart: &RestartInvocation,
    _gateway_protocol: u32,
    _registration_key: Option<&SecretString>,
) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "envd automatic upgrade is supported only on Unix",
    )
}

/// Send `registration/goodbye` and the close frame, within a short bound.
async fn say_goodbye(writer: &mut ControlWriter) {
    let goodbye = Notification::new(
        RegistrationGoodbye::NAME,
        &GoodbyeParams {
            reason: GoodbyeReason::Shutdown,
        },
    )
    .expect("goodbye params encode");
    let _ = tokio::time::timeout(GOODBYE_TIMEOUT, async {
        writer.send(Message::Text(goodbye.to_text().into())).await?;
        writer.close().await
    })
    .await;
}

fn answer(answers: &mpsc::Sender<String>, response: Response) {
    if answers.try_send(response.to_text()).is_err() {
        tracing_line("dropping an answer to the gateway: the control writer is backed up");
    }
}

/// The next text frame before admission. Control frames are skipped; a
/// binary frame, a close or a transport error ends the session.
async fn next_text(reader: &mut ControlReader) -> Result<String, SessionEnd> {
    loop {
        match reader.next().await {
            Some(Ok(Message::Text(text))) => return Ok(text.as_str().to_owned()),
            Some(Ok(Message::Binary(_))) => {
                return Err(SessionEnd::Disconnected(
                    "the gateway sent a binary frame".to_owned(),
                ));
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(SessionEnd::Disconnected(
                    "gateway closed before admission".to_owned(),
                ));
            }
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(SessionEnd::Disconnected(error.to_string())),
        }
    }
}

/// Wait for the register response. Until it arrives the gateway may send
/// nothing else: any request or notification breaks the frame rules.
async fn register_response(reader: &mut ControlReader) -> Result<Response, SessionEnd> {
    loop {
        let text = next_text(reader).await?;
        match jsonrpc::parse(&text) {
            Ok(RpcMessage::Response(response)) if response.id == Some(Id::Number(REGISTER_ID)) => {
                return Ok(response);
            }
            Ok(RpcMessage::Response(response)) => tracing_line(&format!(
                "ignoring a gateway response to id {}",
                response
                    .id
                    .map_or_else(|| "null".to_owned(), |id| id.to_string())
            )),
            Ok(RpcMessage::Request(request)) => {
                return Err(SessionEnd::Disconnected(format!(
                    "the gateway sent {} before the register response",
                    request.method
                )));
            }
            Ok(RpcMessage::Notification(notification)) => {
                return Err(SessionEnd::Disconnected(format!(
                    "the gateway sent {} before the register response",
                    notification.method
                )));
            }
            Err(rejection) => {
                return Err(SessionEnd::Disconnected(format!(
                    "the gateway sent an invalid frame: {}",
                    rejection.error
                )));
            }
        }
    }
}

/// Dial a gateway route with the protocol's message limits, optionally
/// presenting a bearer token.
async fn dial(
    url: &str,
    bearer: Option<&SecretString>,
    tls: &TlsSettings,
) -> Result<ControlSocket> {
    let mut request = url.into_client_request().context("gateway URL")?;
    request.headers_mut().insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("lightspeed-envd/", env!("CARGO_PKG_VERSION"))),
    );
    if let Some(token) = bearer {
        let mut value =
            HeaderValue::from_str(&format!("Bearer {}", token.expose())).context("token header")?;
        value.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    let (socket, _) = connect_async_tls_with_config(
        request,
        Some(websocket_config()),
        false,
        Some(tls.connector()),
    )
    .await
    .map_err(|error| describe_connect_error(url, error))?;
    Ok(socket)
}

/// A gateway that answers the WebSocket upgrade with an HTTP status is
/// reachable but not serving registration; say so instead of leaking a raw
/// handshake error. Still retried, because a deploy can produce the same
/// answer for a moment.
fn describe_connect_error(
    url: &str,
    error: tokio_tungstenite::tungstenite::Error,
) -> anyhow::Error {
    if let tokio_tungstenite::tungstenite::Error::Http(response) = &error {
        let status = response.status();
        if status == 404 {
            return anyhow!(
                "{url} does not serve environment registration (HTTP 404): point LIGHTSPEED_ENVD_GATEWAY_URL at a Lightspeed process running the environment-gateway role"
            );
        }
        return anyhow!("environment gateway {url} answered HTTP {status} instead of upgrading");
    }
    anyhow::Error::new(error).context(format!("connect to environment gateway {url}"))
}

/// Refuse to start when the receipt cannot be written: a daemon admitted
/// without its receipt would be unusable to whoever waits for it.
fn check_receipt_path(path: &Path) -> Result<()> {
    let parent = receipt_parent(path);
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("create receipt dir {}", parent.display()))?;
    let probe = parent.join(format!(
        ".{}.probe.{}",
        path.file_name()
            .map_or_else(|| "receipt".into(), |name| name.to_string_lossy()),
        std::process::id()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .with_context(|| format!("receipt dir {} is not writable", parent.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn receipt_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Write the receipt atomically (when a path is configured) and log it once.
/// It carries ids only; never the registration key or the private key.
fn announce_receipt(receipt: &RegistrationReceipt, path: Option<&Path>) -> Result<()> {
    let json = serde_json::to_string(receipt)?;
    if let Some(path) = path {
        let parent = receipt_parent(path);
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("create receipt dir {}", parent.display()))?;
        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&temp, format!("{json}\n"))
            .with_context(|| format!("write receipt {}", temp.display()))?;
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            bail!("install receipt {}: {error}", path.display());
        }
    }
    tracing_line(&format!("registered {json}"));
    Ok(())
}

/// TLS trust for the gateway: the bundled WebPKI roots, plus an operator
/// supplied PEM bundle for gateways behind a private CA.
///
/// The client config always names its crypto provider. A workspace build
/// links two rustls providers, so a config that relied on the process
/// default would panic on the first dial; the daemon never hands the
/// WebSocket client a `None` connector that would build such a config.
#[derive(Clone)]
struct TlsSettings {
    config: Arc<rustls::ClientConfig>,
}

impl TlsSettings {
    fn from_config(ca_file: Option<&Path>) -> Result<Self> {
        use rustls_pki_types::{CertificateDer, pem::PemObject as _};
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(ca_file) = ca_file {
            let mut added = 0usize;
            for certificate in CertificateDer::pem_file_iter(ca_file)
                .with_context(|| format!("read CA file {}", ca_file.display()))?
            {
                let certificate =
                    certificate.with_context(|| format!("parse CA file {}", ca_file.display()))?;
                roots
                    .add(certificate)
                    .with_context(|| format!("add trust anchor from {}", ca_file.display()))?;
                added += 1;
            }
            if added == 0 {
                bail!("CA file {} holds no certificates", ca_file.display());
            }
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .context("TLS protocol versions")?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
        })
    }

    fn connector(&self) -> Connector {
        Connector::Rustls(self.config.clone())
    }
}

fn jittered(base: Duration) -> Duration {
    use rand::RngExt as _;
    let factor = rand::rng().random_range(0.75_f64..=1.25_f64);
    base.mul_f64(factor)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use environment_protocol::{
        error::{BusyReason, PermissionReason},
        registration::{ChallengeNonce, signed_registration_message},
        routes::{CONNECT_PATH, DATA_PATH, canonical_origin},
    };
    use serde_json::{Value, json};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{
        accept_hdr_async_with_config,
        tungstenite::handshake::server::{Request as HttpRequest, Response as HttpResponse},
    };

    use super::*;
    use crate::{config::OnClosed, test_support::runtime_with};

    type GatewaySocket = WebSocketStream<TcpStream>;

    /// A scripted gateway on a loopback port.
    struct FakeGateway {
        listener: TcpListener,
        connect_url: String,
    }

    struct Accepted {
        socket: GatewaySocket,
        path: String,
        authorization: Option<String>,
    }

    impl FakeGateway {
        async fn start(prefix: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let connect_url = format!(
                "ws://{}{prefix}{CONNECT_PATH}",
                listener.local_addr().unwrap()
            );
            Self {
                listener,
                connect_url,
            }
        }

        // The callback's error type is fixed by tungstenite's `Callback`.
        #[allow(clippy::result_large_err)]
        async fn accept(&self) -> Accepted {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(10), self.listener.accept())
                .await
                .expect("the daemon dials in time")
                .unwrap();
            let seen = Arc::new(Mutex::new((String::new(), None)));
            let record = seen.clone();
            let socket = accept_hdr_async_with_config(
                stream,
                move |request: &HttpRequest, response: HttpResponse| {
                    let authorization = request
                        .headers()
                        .get(AUTHORIZATION)
                        .map(|value| value.to_str().unwrap().to_owned());
                    *record.lock().unwrap() = (request.uri().path().to_owned(), authorization);
                    Ok(response)
                },
                Some(websocket_config()),
            )
            .await
            .unwrap();
            let (path, authorization) = seen.lock().unwrap().clone();
            Accepted {
                socket,
                path,
                authorization,
            }
        }
    }

    async fn send(socket: &mut GatewaySocket, frame: Value) {
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
    }

    async fn recv(socket: &mut GatewaySocket) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
                .await
                .expect("a frame in time")
                .expect("open")
                .expect("frame");
            if let Message::Text(text) = frame {
                return serde_json::from_str(text.as_str()).unwrap();
            }
        }
    }

    async fn challenge(socket: &mut GatewaySocket, nonce: [u8; 32]) {
        send(
            socket,
            json!({
                "jsonrpc": "2.0",
                "method": "registration/challenge",
                "params": {
                    "protocolVersion": ENVD_PROTOCOL_VERSION,
                    "nonce": ChallengeNonce::from_bytes(nonce).to_hex(),
                    "addedLater": {"x": 1},
                },
            }),
        )
        .await;
    }

    fn receipt(admission: &str) -> Value {
        json!({
            "environmentId": "environment_a",
            "daemonId": "daemon_a",
            "controlConnectionId": "connection_a",
            "identityMode": "ephemeral",
            "admission": admission,
        })
    }

    async fn admit(socket: &mut GatewaySocket, register: &Value, heartbeat_ms: u64) {
        send(
            socket,
            json!({
                "jsonrpc": "2.0",
                "id": register["id"],
                "result": {"receipt": receipt("created"), "heartbeatIntervalMs": heartbeat_ms},
            }),
        )
        .await;
    }

    async fn refuse(socket: &mut GatewaySocket, register: &Value, error: ProtocolError) {
        send(
            socket,
            Response::failure(
                Some(serde_json::from_value(register["id"].clone()).unwrap()),
                error,
            )
            .to_value(),
        )
        .await;
    }

    fn daemon(
        gateway: &FakeGateway,
        adjust: impl FnOnce(&mut RegistrationConfig),
    ) -> (tempfile::TempDir, DaemonRuntime, RegistrationConfig) {
        let mut registration = RegistrationConfig::new(
            gateway.connect_url.clone(),
            crate::upgrade::resolve_discovery_url(Some(&gateway.connect_url), None).unwrap(),
        );
        registration.registration_key = Some(SecretString::new("lsrk_test"));
        adjust(&mut registration);
        let config_registration = registration.clone();
        let (temp, runtime) = runtime_with(move |config| {
            config.registration = Some(config_registration);
        });
        (temp, runtime, registration)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registers_with_an_origin_bound_signature_and_drops_the_key_once_admitted() {
        let gateway = FakeGateway::start("/ls").await;
        let receipt_dir = tempfile::tempdir().unwrap();
        let receipt_path = receipt_dir.path().join("out").join("registration.json");
        let (_temp, runtime, registration) = daemon(&gateway, |registration| {
            registration.receipt_path = Some(receipt_path.clone());
        });
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));

        let mut first = gateway.accept().await;
        assert_eq!(first.path, format!("/ls{CONNECT_PATH}"));
        let nonce = [5u8; 32];
        challenge(&mut first.socket, nonce).await;
        let register = recv(&mut first.socket).await;
        assert_eq!(register["method"], "registration/register");
        let params = RegisterParams::decode(&register["params"]).expect("valid register");
        assert_eq!(params.protocol_version, ENVD_PROTOCOL_VERSION);
        assert_eq!(
            params.registration_key.as_ref().map(SecretString::expose),
            Some("lsrk_test")
        );
        let origin = canonical_origin(&gateway.connect_url).unwrap();
        let key =
            ed25519_dalek::VerifyingKey::from_bytes(params.daemon_public_key.as_bytes()).unwrap();
        let signature = ed25519_dalek::Signature::from_bytes(params.signature.as_bytes());
        key.verify_strict(
            &signed_registration_message(&ChallengeNonce::from_bytes(nonce), &origin),
            &signature,
        )
        .expect("the signature binds the nonce and the dialed origin");
        admit(&mut first.socket, &register, 30_000).await;

        // The receipt is written once admitted.
        for _ in 0..200 {
            if receipt_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let written: RegistrationReceipt =
            serde_json::from_str(&std::fs::read_to_string(&receipt_path).unwrap()).unwrap();
        assert_eq!(written.daemon_id.as_str(), "daemon_a");

        // A reconnect proves the identity by its key alone.
        drop(first);
        let mut second = gateway.accept().await;
        challenge(&mut second.socket, [6u8; 32]).await;
        let again = recv(&mut second.socket).await;
        let again = RegisterParams::decode(&again["params"]).unwrap();
        assert!(again.registration_key.is_none(), "the key is never resent");
        assert_eq!(again.daemon_public_key, params.daemon_public_key);

        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn open_stream_dials_the_derived_data_route_with_the_token() {
        let gateway = FakeGateway::start("").await;
        let (_temp, runtime, registration) = daemon(&gateway, |_| {});
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));
        let mut control = gateway.accept().await;
        challenge(&mut control.socket, [1u8; 32]).await;
        let register = recv(&mut control.socket).await;
        admit(&mut control.socket, &register, 30_000).await;

        send(
            &mut control.socket,
            json!({
                "jsonrpc": "2.0",
                "id": "g1",
                "method": "registration/openStream",
                "params": {"token": "stream-token", "stream": {"kind": "dataProtocol"}},
            }),
        )
        .await;
        let mut stream = gateway.accept().await;
        assert_eq!(stream.path, DATA_PATH);
        assert_eq!(stream.authorization.as_deref(), Some("Bearer stream-token"));
        let answer = recv(&mut control.socket).await;
        assert_eq!(answer["id"], "g1");
        assert_eq!(answer["result"], json!({}));

        // The stream serves the data plane.
        send(
            &mut stream.socket,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": ENVD_PROTOCOL_VERSION, "clientInfo": {"name": "gateway-test"},
            }}),
        )
        .await;
        let initialized = recv(&mut stream.socket).await;
        assert_eq!(initialized["result"]["plane"], "data");

        // An unknown gateway request is answered, and the session stays.
        send(
            &mut control.socket,
            json!({"jsonrpc": "2.0", "id": "g2", "method": "registration/somethingNew"}),
        )
        .await;
        let unknown: ProtocolError =
            serde_json::from_value(recv(&mut control.socket).await["error"].clone()).unwrap();
        assert_eq!(
            unknown.kind(),
            environment_protocol::error::ProtocolErrorKind::MethodNotFound
        );
        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_silent_gateway_is_dropped_after_three_heartbeat_intervals() {
        let gateway = FakeGateway::start("").await;
        let (_temp, runtime, registration) = daemon(&gateway, |_| {});
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));
        let mut first = gateway.accept().await;
        challenge(&mut first.socket, [2u8; 32]).await;
        let register = recv(&mut first.socket).await;
        let admitted_at = Instant::now();
        admit(&mut first.socket, &register, 1_000).await;
        // Keep the socket open but silent: only the deadline can end it.
        let _held = first;
        let _second = gateway.accept().await;
        let elapsed = admitted_at.elapsed();
        assert!(
            elapsed >= Duration::from_millis(2_900),
            "reconnected after {elapsed:?}, before the deadline"
        );
        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_notifications_end_the_process_with_distinct_statuses() {
        for (reason, status) in [("superseded", 5), ("environmentClosed", 4)] {
            let gateway = FakeGateway::start("").await;
            let (_temp, runtime, registration) = daemon(&gateway, |_| {});
            let task = tokio::spawn(run_outbound(runtime.clone(), registration));
            let mut control = gateway.accept().await;
            challenge(&mut control.socket, [3u8; 32]).await;
            let register = recv(&mut control.socket).await;
            admit(&mut control.socket, &register, 30_000).await;
            send(
                &mut control.socket,
                json!({"jsonrpc": "2.0", "method": "registration/closed", "params": {
                    "reason": reason, "message": "another process with this identity connected",
                }}),
            )
            .await;
            let exit = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("the daemon ends")
                .unwrap();
            assert_eq!(exit.status(), status, "{reason}: {exit}");
        }
    }

    /// Close the session of a freshly dialed control connection: admit and
    /// send `registration/closed`, or refuse admission, both as
    /// `environmentClosed`.
    async fn close_environment(gateway: &FakeGateway, by_notification: bool) -> Accepted {
        let mut control = gateway.accept().await;
        challenge(&mut control.socket, [9u8; 32]).await;
        let register = recv(&mut control.socket).await;
        if by_notification {
            admit(&mut control.socket, &register, 30_000).await;
            send(
                &mut control.socket,
                json!({"jsonrpc": "2.0", "method": "registration/closed", "params": {
                    "reason": "environmentClosed",
                }}),
            )
            .await;
        } else {
            refuse(
                &mut control.socket,
                &register,
                ProtocolError::failed_precondition(PreconditionReason::EnvironmentClosed, "closed"),
            )
            .await;
        }
        control
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_closed_environment_exits_or_parks_as_configured() {
        for by_notification in [true, false] {
            let gateway = FakeGateway::start("").await;
            let (_temp, runtime, _) = daemon(&gateway, |_| {});
            let server = tokio::spawn(crate::server::run(runtime.clone()));
            let _control = close_environment(&gateway, by_notification).await;
            let exit = tokio::time::timeout(Duration::from_secs(10), server)
                .await
                .expect("the daemon ends")
                .unwrap();
            assert_eq!(exit.status(), 4, "exit by default: {exit}");

            let gateway = FakeGateway::start("").await;
            let (_temp, runtime, _) = daemon(&gateway, |registration| {
                registration.on_closed = OnClosed::Park;
            });
            let server = tokio::spawn(crate::server::run(runtime.clone()));
            let _control = close_environment(&gateway, by_notification).await;
            let redial =
                tokio::time::timeout(Duration::from_millis(1_500), gateway.listener.accept()).await;
            assert!(redial.is_err(), "a parked daemon never dials again");
            assert!(!server.is_finished(), "a parked daemon keeps running");
            runtime.shutdown().cancel();
            let exit = tokio::time::timeout(Duration::from_secs(10), server)
                .await
                .expect("the parked daemon stops on a signal")
                .unwrap();
            assert_eq!(exit.status(), 0, "{exit}");
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn parking_also_stops_the_passive_listener() {
        let gateway = FakeGateway::start("").await;
        let socket_dir = tempfile::tempdir().unwrap();
        let socket = socket_dir.path().join("envd.sock");
        let mut registration = RegistrationConfig::new(
            gateway.connect_url.clone(),
            crate::upgrade::resolve_discovery_url(Some(&gateway.connect_url), None).unwrap(),
        );
        registration.on_closed = OnClosed::Park;
        let listen = socket.clone();
        let (_temp, runtime) = runtime_with(move |config| {
            config.registration = Some(registration);
            config.listen = Some(crate::config::ListenAddr::Unix(listen));
        });
        let server = tokio::spawn(crate::server::run(runtime.clone()));
        let _control = close_environment(&gateway, true).await;
        let mut stopped = false;
        for _ in 0..500 {
            if socket.exists() && tokio::net::UnixStream::connect(&socket).await.is_err() {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(stopped, "the listener stopped accepting");
        assert!(!server.is_finished(), "a parked daemon keeps running");
        runtime.shutdown().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("the parked daemon stops on a signal")
            .unwrap();
        assert_eq!(exit.status(), 0, "{exit}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn register_errors_are_terminal_unless_retryable() {
        let terminal = [
            (
                ProtocolError::permission_denied(
                    Some(PermissionReason::InvalidRegistrationKey),
                    "unknown key",
                ),
                3,
            ),
            (
                ProtocolError::failed_precondition(PreconditionReason::EnvironmentClosed, "closed"),
                4,
            ),
            (
                ProtocolError::invalid_request("implementation.version is too long"),
                3,
            ),
        ];
        for (error, status) in terminal {
            let gateway = FakeGateway::start("").await;
            let (_temp, runtime, registration) = daemon(&gateway, |_| {});
            let task = tokio::spawn(run_outbound(runtime.clone(), registration));
            let mut control = gateway.accept().await;
            challenge(&mut control.socket, [4u8; 32]).await;
            let register = recv(&mut control.socket).await;
            refuse(&mut control.socket, &register, error.clone()).await;
            let exit = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("the daemon ends")
                .unwrap();
            assert_eq!(exit.status(), status, "{error}");
        }

        let gateway = FakeGateway::start("").await;
        let (_temp, runtime, registration) = daemon(&gateway, |_| {});
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));
        let mut control = gateway.accept().await;
        challenge(&mut control.socket, [4u8; 32]).await;
        let register = recv(&mut control.socket).await;
        refuse(
            &mut control.socket,
            &register,
            ProtocolError::busy(Some(BusyReason::RateLimited), Some(1_500), "slow down"),
        )
        .await;
        let refused_at = Instant::now();
        let _again = gateway.accept().await;
        assert!(
            refused_at.elapsed() >= Duration::from_millis(1_400),
            "the retry hint is honoured"
        );
        assert!(!task.is_finished());
        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_gateway_request_before_admission_breaks_the_session() {
        let gateway = FakeGateway::start("").await;
        let (_temp, runtime, registration) = daemon(&gateway, |_| {});
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));
        let mut control = gateway.accept().await;
        challenge(&mut control.socket, [7u8; 32]).await;
        let _register = recv(&mut control.socket).await;
        send(
            &mut control.socket,
            json!({"jsonrpc": "2.0", "id": "early", "method": "registration/openStream", "params": {
                "token": "t", "stream": {"kind": "dataProtocol"},
            }}),
        )
        .await;
        let mut ended = false;
        while let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_secs(5), control.socket.next()).await
        {
            match frame {
                Ok(Message::Close(_)) | Err(_) => {
                    ended = true;
                    break;
                }
                Ok(Message::Text(text)) => panic!("the daemon answered: {text}"),
                Ok(_) => {}
            }
        }
        assert!(ended, "the daemon closed the connection");
        let _reconnected = gateway.accept().await;
        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_says_goodbye_and_exits_cleanly() {
        let gateway = FakeGateway::start("").await;
        let (_temp, runtime, registration) = daemon(&gateway, |_| {});
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));
        let mut control = gateway.accept().await;
        challenge(&mut control.socket, [8u8; 32]).await;
        let register = recv(&mut control.socket).await;
        admit(&mut control.socket, &register, 30_000).await;
        // Give the daemon a moment to enter its admitted loop.
        tokio::time::sleep(Duration::from_millis(100)).await;
        runtime.shutdown().cancel();
        let goodbye = recv(&mut control.socket).await;
        assert_eq!(goodbye["method"], "registration/goodbye");
        assert_eq!(goodbye["params"], json!({"reason": "shutdown"}));
        assert!(goodbye.get("id").is_none());
        let exit = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the daemon ends")
            .unwrap();
        assert_eq!(exit.status(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_gateway_of_another_version_without_auto_upgrade_is_terminal() {
        let gateway = FakeGateway::start("").await;
        let (_temp, runtime, registration) = daemon(&gateway, |_| {});
        let discovery = registration.discovery_url.clone();
        let task = tokio::spawn(run_outbound(runtime.clone(), registration));
        let mut control = gateway.accept().await;
        send(
            &mut control.socket,
            json!({"jsonrpc": "2.0", "method": "registration/challenge", "params": {
                "protocolVersion": ENVD_PROTOCOL_VERSION + 1, "nonce": "not even hex",
            }}),
        )
        .await;
        let exit = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the daemon ends")
            .unwrap();
        assert_eq!(exit.status(), 3);
        let DaemonExit::Refused(message) = exit else {
            panic!("refused");
        };
        assert!(message.contains(&discovery));
        assert!(message.contains("lightspeed-envd upgrade"));
    }

    #[test]
    fn an_upgrade_runs_once_per_target_version() {
        let mut registration = RegistrationConfig::new(
            "wss://gateway.example/environment-gateway/connect".to_owned(),
            "https://gateway.example/.well-known/lightspeed-envd".to_owned(),
        );
        assert!(
            upgrade_refusal(&registration, Some(4)).is_some(),
            "not opted in"
        );
        assert!(
            upgrade_refusal(&registration, None).is_some(),
            "not opted in"
        );
        registration.auto_upgrade = Some(RestartInvocation {
            executable: PathBuf::from("/usr/local/bin/lightspeed-envd"),
            arg0: "lightspeed-envd".into(),
            args: Vec::new(),
        });
        assert!(upgrade_refusal(&registration, Some(4)).is_none());
        assert!(
            upgrade_refusal(&registration, None).is_none(),
            "an unreadable first frame upgrades to whatever discovery publishes"
        );
        registration.upgrade_attempted_for = Some(4);
        assert!(upgrade_refusal(&registration, Some(4)).is_some());
        assert!(
            upgrade_refusal(&registration, Some(5)).is_none(),
            "a later bump upgrades again"
        );
        assert!(
            upgrade_refusal(&registration, None).is_some(),
            "a replacement that still cannot read the first frame stops"
        );
    }

    #[test]
    fn a_replacement_never_inherits_a_registration_key_argument() {
        let args: Vec<std::ffi::OsString> = [
            "--registration-key",
            "lsrk_secret",
            "--gateway-url",
            "wss://g/environment-gateway/connect",
            "--registration-key-file=/run/key",
            "--registration-name",
            "n",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let kept: Vec<String> = replacement_args(&args)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "--gateway-url",
                "wss://g/environment-gateway/connect",
                "--registration-name",
                "n"
            ]
        );
    }

    #[test]
    fn retryable_refusals_carry_their_hint() {
        let SessionEnd::Retry { retry_after, .. } = refusal(ProtocolError::busy(
            Some(BusyReason::RateLimited),
            Some(250),
            "x",
        )) else {
            panic!("retry");
        };
        assert_eq!(retry_after, Some(Duration::from_millis(250)));
        assert!(matches!(
            refusal(ProtocolError::unavailable("store down")),
            SessionEnd::Retry { .. }
        ));
        assert!(matches!(
            refusal(ProtocolError::version_mismatch(4, 3)),
            SessionEnd::ProtocolMismatch(4)
        ));
        assert!(matches!(
            refusal(ProtocolError::expired("key expired")),
            SessionEnd::Exit(DaemonExit::Refused(_))
        ));
    }

    #[test]
    fn receipts_are_written_atomically_and_contain_only_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("out").join("registration.json");
        check_receipt_path(&path).expect("writable");
        let receipt: RegistrationReceipt = serde_json::from_value(receipt("reconnected")).unwrap();
        announce_receipt(&receipt, Some(&path)).expect("write");
        let written: RegistrationReceipt =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(written, receipt);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_receipt_dir_is_refused_at_startup() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().expect("tempdir");
        let locked = temp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let refused = check_receipt_path(&locked.join("registration.json"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Root ignores directory permissions.
        if !rustix::process::geteuid().is_root() {
            assert!(refused.is_err());
        }
    }

    /// A self-signed EC certificate, valid 2026-2036, used only as PEM input.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBmTCCAT+gAwIBAgIUGlIKCYXc2/JUu1OUJBgh5a7gJ4cwCgYIKoZIzj0EAwIw
IjEgMB4GA1UEAwwXbGlnaHRzcGVlZC1lbnZkLXRlc3QtY2EwHhcNMjYwOTAzMTA1
MjUwWhcNMzYwODMxMTA1MjUwWjAiMSAwHgYDVQQDDBdsaWdodHNwZWVkLWVudmQt
dGVzdC1jYTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABHdyQgLSBwg3QZhQHj3S
AnnUyqMnO2ZIP80Q4un3Fns7yFLOhs6YD5qu8hVSutuxSAbCAlM3Uxujy83VYJNe
cxajUzBRMB0GA1UdDgQWBBRs3ltP/Ch8oC/RpiwqRAEBpGeuhzAfBgNVHSMEGDAW
gBRs3ltP/Ch8oC/RpiwqRAEBpGeuhzAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49
BAMCA0gAMEUCICgTw9frlQLXbAvhAXlMwBH6gCkV3ypx4U93yaMHghBDAiEAi4tZ
fIEYbSNa76Ox9es7E5q8U7/XDqkEOn/kC4eSA5Q=
-----END CERTIFICATE-----
";

    #[test]
    fn missing_ca_file_and_empty_bundle_fail_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(TlsSettings::from_config(Some(&temp.path().join("missing.pem"))).is_err());
        let empty = temp.path().join("empty.pem");
        std::fs::write(&empty, "").expect("write");
        assert!(TlsSettings::from_config(Some(&empty)).is_err());
    }

    /// Both trust configurations hand the WebSocket client an explicit
    /// rustls connector. This is what keeps a workspace build, which links
    /// two crypto providers, from reaching tungstenite's default connector
    /// and panicking on the first dial.
    #[test]
    fn every_trust_configuration_carries_an_explicit_connector() {
        let bundled = TlsSettings::from_config(None).expect("bundled roots");
        assert!(matches!(bundled.connector(), Connector::Rustls(_)));

        let temp = tempfile::tempdir().expect("tempdir");
        let ca_file = temp.path().join("ca.pem");
        std::fs::write(&ca_file, TEST_CA_PEM).expect("write");
        let extended = TlsSettings::from_config(Some(&ca_file)).expect("operator roots");
        assert!(matches!(extended.connector(), Connector::Rustls(_)));
    }
}
