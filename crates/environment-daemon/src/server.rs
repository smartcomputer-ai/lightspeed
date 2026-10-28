//! Serving the data plane: the passive listener and the connection loop
//! every data socket runs, accepted or reverse-dialed.
//!
//! Per connection, one reader parses frames with the shared envelope and
//! enforces the handshake; one writer task owns the socket sink. Requests
//! after `initialize` run as their own tasks, up to
//! `limits.maxConcurrentRequests` at once, so responses leave in completion
//! order and the reader never waits on the writer. When the reader ends,
//! every pending request of the connection is cancelled and its reply,
//! with any after-write hook, is dropped.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context as _;
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    error::{BusyReason, ProtocolError},
    jsonrpc::{
        self, CANCEL_REQUEST_METHOD, CancelRequestParams, Id, Message as RpcMessage, Request,
        Response,
    },
    limits::MAX_MESSAGE_BYTES,
    manifest::DataInitialize,
};
use futures_util::{Sink, SinkExt as _, StreamExt as _};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, mpsc},
};
use tokio_tungstenite::{
    WebSocketStream, accept_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

use crate::{
    DaemonRuntime,
    config::{ListenAddr, OnClosed},
    dispatch::{self, ConnectionInfo, RequestContext, WrittenHook},
};

/// How the daemon process ends. Each outcome has its own exit status so an
/// orchestrator can tell a deliberate stop from a refusal or a cloned
/// identity.
#[derive(Debug)]
pub enum DaemonExit {
    /// `SIGTERM` or `SIGINT`; a registered daemon said goodbye first, and
    /// a parked one had already stopped registering.
    Shutdown,
    /// Local configuration, state or receipt-file error.
    Configuration(anyhow::Error),
    /// The gateway refused registration for good, or it speaks another
    /// protocol and no automatic upgrade succeeded.
    Refused(String),
    /// The gateway closed the identity's environment, and the daemon is
    /// configured to exit rather than park.
    EnvironmentClosed,
    /// Another process with this daemon identity connected and won.
    Superseded { daemon_public_key: String },
    /// An unexpected failure.
    Failed(anyhow::Error),
}

impl DaemonExit {
    pub fn status(&self) -> u8 {
        match self {
            Self::Shutdown => 0,
            Self::Failed(_) => 1,
            Self::Configuration(_) => 2,
            Self::Refused(_) => 3,
            Self::EnvironmentClosed => 4,
            Self::Superseded { .. } => 5,
        }
    }
}

impl std::fmt::Display for DaemonExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shutdown => formatter.write_str("shut down"),
            Self::Configuration(error) => write!(formatter, "configuration error: {error:#}"),
            Self::Refused(message) => write!(formatter, "registration refused: {message}"),
            Self::EnvironmentClosed => formatter.write_str("the environment was closed"),
            Self::Superseded { daemon_public_key } => write!(
                formatter,
                "superseded: another process with daemon public key {daemon_public_key} connected; \
                 two daemons share one state directory"
            ),
            Self::Failed(error) => write!(formatter, "failed: {error:#}"),
        }
    }
}

/// The WebSocket limits of every envd socket: one message is one frame of
/// at most `MAX_MESSAGE_BYTES`.
pub(crate) fn websocket_config() -> WebSocketConfig {
    let limit = usize::try_from(MAX_MESSAGE_BYTES).unwrap_or(usize::MAX);
    WebSocketConfig::default()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit))
}

/// Serve the daemon until it stops: the passive listener when one is
/// configured, the outbound registration loop when a gateway URL is
/// configured, or both. A daemon configured to park once its environment
/// is closed stops both and waits for a deliberate stop instead of exiting.
pub async fn run(runtime: DaemonRuntime) -> DaemonExit {
    let shutdown = runtime.shutdown().clone();
    let on_closed = runtime
        .config()
        .registration
        .as_ref()
        .map_or(OnClosed::Exit, |registration| registration.on_closed);
    match (serve(runtime).await, on_closed) {
        (DaemonExit::EnvironmentClosed, OnClosed::Park) => park(&shutdown).await,
        (exit, _) => exit,
    }
}

/// Wait for `SIGTERM` or `SIGINT` after the registration loop and the
/// listener have ended. Data connections already open run until then.
async fn park(shutdown: &CancellationToken) -> DaemonExit {
    tracing_line("the environment was closed; parked until SIGTERM or SIGINT");
    shutdown.cancelled().await;
    DaemonExit::Shutdown
}

async fn serve(runtime: DaemonRuntime) -> DaemonExit {
    let listen = runtime.config().listen.clone();
    let registration = runtime.config().registration.clone();
    match (listen, registration) {
        (Some(listen), None) => run_listener(runtime, listen).await,
        (None, Some(registration)) => {
            crate::registration::run_outbound(runtime, registration).await
        }
        (Some(listen), Some(registration)) => {
            let listener = run_listener(runtime.clone(), listen);
            let outbound = crate::registration::run_outbound(runtime, registration);
            tokio::pin!(listener, outbound);
            tokio::select! {
                exit = &mut listener => match exit {
                    // The registration loop says goodbye on the same token.
                    DaemonExit::Shutdown => outbound.await,
                    other => other,
                },
                exit = &mut outbound => exit,
            }
        }
        (None, None) => {
            DaemonExit::Configuration(anyhow::anyhow!("configure a listener or a gateway URL"))
        }
    }
}

/// Cancel the runtime's shutdown token on the first `SIGTERM` or `SIGINT`.
pub fn spawn_signal_listener(runtime: &DaemonRuntime) -> anyhow::Result<()> {
    let shutdown = runtime.shutdown().clone();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
        let mut interrupt = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
        tokio::spawn(async move {
            tokio::select! {
                _ = terminate.recv() => tracing_line("SIGTERM received; shutting down"),
                _ = interrupt.recv() => tracing_line("SIGINT received; shutting down"),
            }
            shutdown.cancel();
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing_line("interrupt received; shutting down");
            shutdown.cancel();
        }
    });
    Ok(())
}

async fn run_listener(runtime: DaemonRuntime, listen: ListenAddr) -> DaemonExit {
    match listen {
        ListenAddr::Tcp(addr) => run_tcp_listener(runtime, addr).await,
        #[cfg(unix)]
        ListenAddr::Unix(path) => run_unix_listener(runtime, path).await,
    }
}

async fn run_tcp_listener(runtime: DaemonRuntime, listen: std::net::SocketAddr) -> DaemonExit {
    let listener = match TcpListener::bind(listen)
        .await
        .context("bind environment daemon listener")
    {
        Ok(listener) => listener,
        Err(error) => return DaemonExit::Configuration(error),
    };
    tracing_line(&format!("listener ready on {listen}"));
    let shutdown = runtime.shutdown().clone();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return DaemonExit::Shutdown,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => serve_accepted(runtime.clone(), stream, peer.to_string()),
                Err(error) => accept_failed(error).await,
            },
        }
    }
}

#[cfg(unix)]
async fn run_unix_listener(runtime: DaemonRuntime, path: std::path::PathBuf) -> DaemonExit {
    let listener = match bind_unix_listener(&path) {
        Ok(listener) => listener,
        Err(error) => return DaemonExit::Configuration(error),
    };
    let label = ListenAddr::Unix(path).to_string();
    tracing_line(&format!("listener ready on {label}"));
    let shutdown = runtime.shutdown().clone();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return DaemonExit::Shutdown,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => serve_accepted(runtime.clone(), stream, label.clone()),
                Err(error) => accept_failed(error).await,
            },
        }
    }
}

/// Accept errors are transient or specific to one connection (descriptor
/// exhaustion, a peer that hung up mid-accept), so the listener keeps serving.
/// The pause stops a persistent error from spinning the loop.
async fn accept_failed(error: std::io::Error) {
    tracing_line(&format!("accept failed: {error}"));
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Bind a Unix-socket listener only this daemon's user can connect to. A
/// socket node left by an earlier run is replaced; a live listener or any
/// other kind of file at the path is not.
#[cfg(unix)]
fn bind_unix_listener(path: &std::path::Path) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;

    remove_stale_socket(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let listener = tokio::net::UnixListener::bind(path)
        .with_context(|| format!("bind environment daemon listener {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restrict {}", path.display()))?;
    Ok(listener)
}

/// Remove a socket node whose listener is gone. Only a refused connection
/// proves that; any other probe failure (such as a socket this user may not
/// connect to) leaves the node alone.
#[cfg(unix)]
fn remove_stale_socket(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt as _;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {}
        Ok(_) => anyhow::bail!("refusing to replace non-socket {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", path.display()));
        }
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => anyhow::bail!("another listener is serving {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
                .with_context(|| format!("remove stale socket {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot tell whether {} is in use", path.display()))
        }
    }
}

fn serve_accepted<S>(runtime: DaemonRuntime, stream: S, peer: String)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(error) = serve_stream(runtime, stream).await {
            tracing_line(&format!("connection {peer} failed: {error:#}"));
        }
    });
}

/// Accept a WebSocket upgrade on an already connected byte stream and serve
/// the data plane on it until the connection ends. In-process callers pair
/// it with `DataClient::connect_initialized_over` on the other end of a
/// `tokio::io::duplex`.
pub async fn serve_stream<S>(runtime: DaemonRuntime, stream: S) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let socket = accept_async_with_config(stream, Some(websocket_config()))
        .await
        .context("websocket handshake")?;
    run_data_connection(&runtime, socket).await;
    Ok(())
}

/// One frame for the writer task: an encoded response and the hook to run
/// once it is on the socket.
struct Outgoing {
    text: String,
    on_written: Option<WrittenHook>,
}

impl Outgoing {
    fn response(response: Response) -> Self {
        Self {
            text: response.to_text(),
            on_written: None,
        }
    }
}

/// Pending requests of one connection by id, each with the token that
/// `$/cancelRequest` and the connection's close cancel.
type PendingRequests = Arc<Mutex<HashMap<Id, CancellationToken>>>;

/// Serve the data protocol on one accepted or dialed socket. Outbound data
/// sockets and passive connections run this same loop.
pub(crate) async fn run_data_connection<S>(runtime: &DaemonRuntime, socket: WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let connection = Arc::new(ConnectionInfo {
        connection_id: runtime.next_connection_id(),
        closed: CancellationToken::new(),
    });
    let max_concurrent = runtime.limits().max_concurrent_requests as usize;
    // Handler replies are bounded by the request permits; the rest of the
    // capacity holds the reader's own answers (framing errors, busy). A
    // peer that fills it without reading is closed rather than buffered.
    let (outgoing, outgoing_rx) = mpsc::channel::<Outgoing>(max_concurrent + 64);
    let (sink, mut stream) = socket.split();
    let writer = tokio::spawn(write_loop(sink, outgoing_rx, connection.closed.clone()));
    let permits = Arc::new(Semaphore::new(max_concurrent));
    let pending: PendingRequests = Arc::default();
    let shutdown = runtime.shutdown().clone();
    let mut session = Session {
        runtime,
        connection: &connection,
        outgoing: &outgoing,
        permits: &permits,
        pending: &pending,
        initialized: false,
    };
    loop {
        let frame = tokio::select! {
            _ = connection.closed.cancelled() => break,
            _ = shutdown.cancelled() => break,
            frame = stream.next() => frame,
        };
        let message = match frame {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                tracing_line(&format!(
                    "connection {} ended: {error}",
                    connection.connection_id
                ));
                break;
            }
            None => break,
        };
        let keep_open = match message {
            Message::Text(text) => session.on_text(text.as_str()),
            Message::Binary(_) => session.answer(Response::failure(
                None,
                ProtocolError::invalid_request("binary frames are not part of the protocol"),
            )),
            // Tungstenite answers pings itself on the read path, ahead of
            // anything the writer has queued.
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => true,
            Message::Close(_) => break,
        };
        if !keep_open {
            break;
        }
    }
    connection.closed.cancel();
    pending
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
    drop(outgoing);
    let _ = writer.await;
}

/// The reader's view of one connection.
struct Session<'a> {
    runtime: &'a DaemonRuntime,
    connection: &'a Arc<ConnectionInfo>,
    outgoing: &'a mpsc::Sender<Outgoing>,
    permits: &'a Arc<Semaphore>,
    pending: &'a PendingRequests,
    initialized: bool,
}

impl Session<'_> {
    /// Handle one text frame; false closes the connection.
    fn on_text(&mut self, text: &str) -> bool {
        match jsonrpc::parse(text) {
            Err(rejection) => match rejection.into_response() {
                Some(response) => self.answer(response),
                None => true,
            },
            Ok(RpcMessage::Request(request)) => self.on_request(request),
            Ok(RpcMessage::Notification(notification)) => {
                if self.initialized
                    && notification.method == CANCEL_REQUEST_METHOD
                    && let Ok(params) = notification.params::<CancelRequestParams>()
                {
                    self.cancel(&params.id);
                }
                true
            }
            // envd never sends requests, so no response can be one it
            // waits for.
            Ok(RpcMessage::Response(response)) => {
                tracing_line(&format!(
                    "connection {} ignored a response to id {}",
                    self.connection.connection_id,
                    response
                        .id
                        .map_or_else(|| "null".to_owned(), |id| id.to_string())
                ));
                true
            }
        }
    }

    fn on_request(&mut self, request: Request) -> bool {
        if !self.initialized {
            if request.method != DataInitialize::NAME {
                return self.answer(Response::failure(
                    Some(request.id),
                    ProtocolError::not_initialized(),
                ));
            }
            let result = initialize(self.runtime, &request);
            self.initialized = result.is_ok();
            return self.answer(Response::from_result(request.id, result));
        }
        if request.method == DataInitialize::NAME {
            return self.answer(Response::failure(
                Some(request.id),
                ProtocolError::invalid_request(
                    "initialize was already answered on this connection",
                ),
            ));
        }
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            return self.answer(Response::failure(
                Some(request.id),
                ProtocolError::busy(
                    Some(BusyReason::RequestLimit),
                    None,
                    format!(
                        "{} requests are already in flight on this connection",
                        self.runtime.limits().max_concurrent_requests
                    ),
                ),
            ));
        };
        let ctx = RequestContext::new(self.connection.clone());
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if pending.contains_key(&request.id) {
                drop(pending);
                return self.answer(Response::failure(
                    Some(request.id),
                    ProtocolError::invalid_request("a request with this id is already in flight"),
                ));
            }
            pending.insert(request.id.clone(), ctx.cancel.clone());
        }
        let runtime = self.runtime.clone();
        let outgoing = self.outgoing.clone();
        let pending = self.pending.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let Request { id, method, params } = request;
            let result = dispatch::handle_request(&runtime, &ctx, &method, params).await;
            pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            if ctx.connection.closed.is_cancelled() {
                return;
            }
            let _ = outgoing.send(encode_reply(id, result)).await;
        });
        true
    }

    fn cancel(&self, id: &Id) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(token) = pending.get(id) {
            token.cancel();
        }
    }

    /// Queue a reader-produced answer without waiting; false when the peer
    /// stopped reading and the queue is full.
    fn answer(&self, response: Response) -> bool {
        match self.outgoing.try_send(Outgoing::response(response)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing_line(&format!(
                    "connection {} closed: the peer does not read its responses",
                    self.connection.connection_id
                ));
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// Encode a handler's result. A success whose encoding would break the
/// message limit becomes `payloadTooLarge`, and its after-write hook is
/// dropped with it: nothing the client never received may be committed.
fn encode_reply(
    id: Id,
    result: Result<dispatch::Reply<serde_json::Value>, ProtocolError>,
) -> Outgoing {
    match result {
        Ok(reply) => {
            let response = Response {
                id: Some(id),
                result: Ok(reply.value),
            };
            let text = response.to_value().to_string();
            if jsonrpc::check_message_size(&text).is_ok() {
                Outgoing {
                    text,
                    on_written: reply.on_written,
                }
            } else {
                Outgoing::response(response)
            }
        }
        Err(error) => Outgoing::response(Response::failure(Some(id), error)),
    }
}

/// `initialize`: the version is compared before the strict decode, so a
/// client of another version learns that rather than which field it sent
/// that this daemon does not know.
fn initialize(
    runtime: &DaemonRuntime,
    request: &Request,
) -> Result<environment_protocol::data::handshake::InitializeResponse, ProtocolError> {
    if let Some(received) = request
        .params
        .get("protocolVersion")
        .and_then(serde_json::Value::as_u64)
        && received != u64::from(ENVD_PROTOCOL_VERSION)
    {
        return Err(ProtocolError::version_mismatch(
            ENVD_PROTOCOL_VERSION,
            u32::try_from(received).unwrap_or(u32::MAX),
        ));
    }
    let _params: environment_protocol::data::handshake::InitializeParams = request.params()?;
    Ok(runtime.initialize_response())
}

/// The connection's only writer: sends frames in the order handlers finish
/// and runs each after-write hook once its frame is on the socket. Stops,
/// dropping whatever is still queued, when the connection closes.
async fn write_loop<Si>(
    mut sink: Si,
    mut queue: mpsc::Receiver<Outgoing>,
    closed: CancellationToken,
) where
    Si: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    loop {
        let next = tokio::select! {
            biased;
            _ = closed.cancelled() => break,
            next = queue.recv() => next,
        };
        let Some(Outgoing { text, on_written }) = next else {
            break;
        };
        let sent = tokio::select! {
            biased;
            _ = closed.cancelled() => break,
            sent = sink.send(Message::Text(text.into())) => sent,
        };
        match sent {
            Ok(()) => {
                if let Some(hook) = on_written {
                    hook();
                }
            }
            Err(_) => {
                closed.cancel();
                break;
            }
        }
    }
    // Sends our close frame, or the reply tungstenite queued for the peer's.
    let _ = tokio::time::timeout(Duration::from_secs(5), sink.close()).await;
}

pub(crate) fn tracing_line(message: &str) {
    eprintln!("lightspeed-envd {message}");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use environment_protocol::{
        error::{ErrorData, ProtocolErrorKind},
        handshake::ImplementationInfo,
    };
    use serde_json::{Value, json};
    use tokio::io::DuplexStream;
    use tokio_tungstenite::client_async_with_config;

    use super::*;
    use crate::test_support::runtime;

    type Client = WebSocketStream<DuplexStream>;

    async fn connect(runtime: &DaemonRuntime) -> Client {
        let (client, server) = tokio::io::duplex(1 << 20);
        tokio::spawn(serve_stream(runtime.clone(), server));
        client_async_with_config("ws://localhost/", client, Some(websocket_config()))
            .await
            .expect("upgrade")
            .0
    }

    async fn send(client: &mut Client, frame: Value) {
        client
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("send");
    }

    async fn next_json(client: &mut Client) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), client.next())
                .await
                .expect("a frame in time")
                .expect("open")
                .expect("frame");
            if let Message::Text(text) = frame {
                return serde_json::from_str(text.as_str()).expect("json");
            }
        }
    }

    fn error_kind(response: &Value) -> ProtocolErrorKind {
        let error: ProtocolError =
            serde_json::from_value(response["error"].clone()).expect("an error response");
        error.kind()
    }

    fn initialize_params(version: u32) -> Value {
        json!({
            "protocolVersion": version,
            "clientInfo": {"name": "envd-server-test"},
        })
    }

    async fn initialized(runtime: &DaemonRuntime) -> Client {
        let mut client = connect(runtime).await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": initialize_params(ENVD_PROTOCOL_VERSION)}),
        )
        .await;
        let response = next_json(&mut client).await;
        assert_eq!(response["result"]["plane"], "data", "{response}");
        client
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_handshake_is_enforced_and_the_connection_survives_every_refusal() {
        let (_temp, runtime) = runtime();
        let mut client = connect(&runtime).await;

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 1, "method": "env/getIdle"}),
        )
        .await;
        let early = next_json(&mut client).await;
        assert_eq!(early["id"], 1);
        assert_eq!(error_kind(&early), ProtocolErrorKind::NotInitialized);

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": ENVD_PROTOCOL_VERSION + 1, "clientInfo": {"name": "x"}, "fromTheFuture": true}}),
        )
        .await;
        let mismatch: ProtocolError =
            serde_json::from_value(next_json(&mut client).await["error"].clone()).unwrap();
        assert_eq!(
            mismatch.data,
            ErrorData::VersionMismatch {
                expected: ENVD_PROTOCOL_VERSION,
                received: ENVD_PROTOCOL_VERSION + 1
            }
        );

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": initialize_params(ENVD_PROTOCOL_VERSION)}),
        )
        .await;
        let response = next_json(&mut client).await;
        assert_eq!(response["id"], 3);
        assert_eq!(response["result"]["protocolVersion"], ENVD_PROTOCOL_VERSION);
        assert_eq!(
            response["result"]["daemonInstanceId"],
            runtime.instance_id().as_str()
        );

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 4, "method": "initialize", "params": initialize_params(ENVD_PROTOCOL_VERSION)}),
        )
        .await;
        assert_eq!(
            error_kind(&next_json(&mut client).await),
            ProtocolErrorKind::InvalidRequest
        );

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 5, "method": "env/getIdle"}),
        )
        .await;
        let idle = next_json(&mut client).await;
        assert!(idle["result"]["idleForMs"].is_u64(), "{idle}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn framing_errors_are_answered_with_null_ids_and_notifications_never() {
        let (_temp, runtime) = runtime();
        let mut client = initialized(&runtime).await;

        client.send(Message::Text("not json".into())).await.unwrap();
        let parse = next_json(&mut client).await;
        assert_eq!(parse["id"], Value::Null);
        assert_eq!(error_kind(&parse), ProtocolErrorKind::ParseError);

        client
            .send(Message::Binary(b"{}".to_vec().into()))
            .await
            .unwrap();
        let binary = next_json(&mut client).await;
        assert_eq!(binary["id"], Value::Null);
        assert_eq!(error_kind(&binary), ProtocolErrorKind::InvalidRequest);

        // A malformed notification and an unknown one: no answer at all.
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": [1]}),
        )
        .await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "method": "x/unknown"}),
        )
        .await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "after", "method": "nope/nothing"}),
        )
        .await;
        let next = next_json(&mut client).await;
        assert_eq!(next["id"], "after");
        assert_eq!(error_kind(&next), ProtocolErrorKind::MethodNotFound);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn params_are_decoded_strictly_and_name_the_field() {
        let (_temp, runtime) = runtime();
        let mut client = initialized(&runtime).await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": 9, "method": "env/getIdle", "params": {"unexpected": 1}}),
        )
        .await;
        let response = next_json(&mut client).await;
        assert_eq!(error_kind(&response), ProtocolErrorKind::InvalidParams);
    }

    /// Every data request in the manifest reaches a handler: none falls
    /// through to `methodNotFound`.
    #[tokio::test(flavor = "current_thread")]
    async fn every_manifest_request_is_dispatched() {
        use environment_protocol::{
            handshake::Plane,
            manifest::{self, Direction, MessageKind},
        };
        let (_temp, runtime) = runtime();
        for spec in manifest::plane_methods(Plane::Data).filter(|spec| {
            spec.kind == MessageKind::Request && spec.direction == Direction::ClientToServer
        }) {
            let result = dispatch::handle_request(
                &runtime,
                &RequestContext::detached(),
                spec.name,
                json!({"__unknownMember": true}),
            )
            .await;
            if let Err(error) = result {
                assert_ne!(
                    error.kind(),
                    ProtocolErrorKind::MethodNotFound,
                    "{} is not dispatched",
                    spec.name
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requests_are_pipelined_and_abortable_waits_are_cancelled() {
        let (_temp, runtime) = runtime();
        let mut client = initialized(&runtime).await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "start", "method": "process/start", "params": {
                "processId": "quiet",
                "command": {"argv": ["/bin/sh", "-c", "sleep 30"], "timeoutMs": 60000},
            }}),
        )
        .await;
        assert!(next_json(&mut client).await["result"]["pid"].is_u64());

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "wait", "method": "process/read", "params": {
                "processId": "quiet", "maxBytes": 1024, "waitMs": 30000, "returnOn": "exit",
            }}),
        )
        .await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "idle", "method": "env/getIdle"}),
        )
        .await;
        let first = next_json(&mut client).await;
        assert_eq!(
            first["id"], "idle",
            "a later request answers first: {first}"
        );

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": "wait"}}),
        )
        .await;
        let cancelled = next_json(&mut client).await;
        assert_eq!(cancelled["id"], "wait");
        let error: ProtocolError = serde_json::from_value(cancelled["error"].clone()).unwrap();
        assert_eq!(error, ProtocolError::cancelled());

        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "kill", "method": "process/terminate", "params": {"processId": "quiet", "signal": "kill"}}),
        )
        .await;
        assert_eq!(next_json(&mut client).await["id"], "kill");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requests_beyond_the_concurrency_ceiling_are_busy() {
        let (_temp, runtime) = runtime();
        let mut client = initialized(&runtime).await;
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "start", "method": "process/start", "params": {
                "processId": "quiet",
                "command": {"argv": ["/bin/sh", "-c", "sleep 30"], "timeoutMs": 60000},
            }}),
        )
        .await;
        next_json(&mut client).await;
        let ceiling = runtime.limits().max_concurrent_requests;
        for index in 0..=ceiling {
            send(
                &mut client,
                json!({"jsonrpc": "2.0", "id": index, "method": "process/read", "params": {
                    "processId": "quiet", "after": 0, "maxBytes": 0, "waitMs": 30000, "returnOn": "exit",
                }}),
            )
            .await;
        }
        let busy = next_json(&mut client).await;
        assert_eq!(busy["id"], ceiling);
        let error: ProtocolError = serde_json::from_value(busy["error"].clone()).unwrap();
        assert!(matches!(
            error.data,
            ErrorData::Busy {
                reason: Some(BusyReason::RequestLimit),
                ..
            }
        ));
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 0}}),
        )
        .await;
        assert_eq!(next_json(&mut client).await["id"], 0);
        send(
            &mut client,
            json!({"jsonrpc": "2.0", "id": "kill", "method": "process/terminate", "params": {"processId": "quiet", "signal": "kill"}}),
        )
        .await;
    }

    /// A cursor read whose connection drops is cancelled and consumes
    /// nothing: the next connection's cursor read still sees every byte.
    #[tokio::test(flavor = "current_thread")]
    async fn a_dropped_connection_consumes_no_output() {
        let (_temp, runtime) = runtime();
        let mut first = initialized(&runtime).await;
        send(
            &mut first,
            json!({"jsonrpc": "2.0", "id": 1, "method": "process/start", "params": {
                "processId": "late",
                "command": {"argv": ["/bin/sh", "-c", "printf early; sleep 1; printf late"], "timeoutMs": 60000},
            }}),
        )
        .await;
        next_json(&mut first).await;
        send(
            &mut first,
            json!({"jsonrpc": "2.0", "id": 2, "method": "process/read", "params": {
                "processId": "late", "maxBytes": 1024, "waitMs": 30000, "returnOn": "exit",
            }}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(first);

        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut second = initialized(&runtime).await;
        send(
            &mut second,
            json!({"jsonrpc": "2.0", "id": 3, "method": "process/read", "params": {
                "processId": "late", "maxBytes": 1024, "waitMs": 10000, "returnOn": "exit",
            }}),
        )
        .await;
        let page = next_json(&mut second).await;
        let text: String = page["result"]["output"]["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .map(|chunk| {
                let bytes: environment_protocol::bytes::ByteChunk =
                    serde_json::from_value(chunk["data"].clone()).unwrap();
                String::from_utf8(bytes.into_inner()).unwrap()
            })
            .collect();
        assert_eq!(text, "earlylate", "{page}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_client_library_completes_the_handshake() {
        let (_temp, runtime) = runtime();
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(serve_stream(runtime.clone(), server_io));
        let options = environment_client::ConnectOptions::new(
            ImplementationInfo::new("envd-server-test").unwrap(),
        );
        let (client, initialize) =
            environment_client::DataClient::connect_initialized_over(client_io, options)
                .await
                .expect("initialized");
        assert_eq!(&initialize.daemon_instance_id, runtime.instance_id());
        client
            .get_idle(&environment_protocol::data::idle::GetIdleParams {})
            .await
            .expect("idle");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_ends_listeners_and_connections() {
        let (_temp, runtime) = runtime();
        let mut client = initialized(&runtime).await;
        runtime.shutdown().cancel();
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match client.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "the daemon closed the connection");
    }

    #[cfg(unix)]
    mod unix_listener {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;
        use crate::test_support::runtime_with;

        #[tokio::test(flavor = "current_thread")]
        async fn serves_the_data_protocol_on_a_private_unix_socket() {
            let socket_dir = tempfile::tempdir().unwrap();
            // The parent does not exist yet: guest tmpfs paths start empty.
            let socket = socket_dir.path().join("run").join("envd.sock");
            let (_temp, runtime) = runtime_with(|config| {
                config.listen = Some(ListenAddr::Unix(socket.clone()));
            });
            let server = tokio::spawn(run(runtime.clone()));
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
            let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);

            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (mut client, _) =
                client_async_with_config("ws://envd/", stream, Some(websocket_config()))
                    .await
                    .unwrap();
            client
                .send(Message::Text(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params(ENVD_PROTOCOL_VERSION)})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let Some(Ok(Message::Text(reply))) = client.next().await else {
                panic!("expected an initialize response");
            };
            let reply: Value = serde_json::from_str(reply.as_str()).unwrap();
            assert_eq!(reply["id"], 1);
            assert_eq!(reply["result"]["protocolVersion"], ENVD_PROTOCOL_VERSION);

            runtime.shutdown().cancel();
            let exit = tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .expect("listener stops")
                .unwrap();
            assert_eq!(exit.status(), 0);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn stale_sockets_are_replaced_but_live_ones_and_files_are_not() {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("envd.sock");

            drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
            assert!(socket.exists(), "a dropped listener leaves its node behind");
            let listener = bind_unix_listener(&socket).expect("stale socket replaced");

            assert!(bind_unix_listener(&socket).is_err(), "live listener kept");
            drop(listener);

            let file = temp.path().join("not-a-socket");
            std::fs::write(&file, b"data").unwrap();
            assert!(bind_unix_listener(&file).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), b"data");
        }

        #[tokio::test(flavor = "current_thread")]
        async fn a_live_socket_that_cannot_be_probed_is_left_in_place() {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("envd.sock");
            let _live = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            // Without write permission, connect fails with EACCES rather than
            // ECONNREFUSED (root bypasses this and sees the live listener).
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o000)).unwrap();

            assert!(bind_unix_listener(&socket).is_err());
            assert!(
                std::fs::symlink_metadata(&socket).is_ok(),
                "live socket kept"
            );
        }
    }

    #[test]
    fn exit_statuses_are_distinct() {
        let statuses = [
            DaemonExit::Shutdown.status(),
            DaemonExit::Failed(anyhow::anyhow!("x")).status(),
            DaemonExit::Configuration(anyhow::anyhow!("x")).status(),
            DaemonExit::Refused("x".to_owned()).status(),
            DaemonExit::EnvironmentClosed.status(),
            DaemonExit::Superseded {
                daemon_public_key: "k".to_owned(),
            }
            .status(),
        ];
        let unique: std::collections::BTreeSet<_> = statuses.iter().collect();
        assert_eq!(unique.len(), statuses.len());
        assert_eq!(statuses, [0, 1, 2, 3, 4, 5]);
    }
}
