//! The multiplexed JSON-RPC connection under both clients.
//!
//! One writer task owns the socket's sink and one reader task its stream.
//! A call registers a oneshot under a fresh integer id, queues its frame
//! for the writer and waits; the reader routes each response to its call
//! by id, in whatever order the server answers. A call that is dropped or
//! runs past its deadline removes its entry, so a late response is
//! discarded, and on the data plane an abortable call also sends
//! `$/cancelRequest`. While any call is pending the writer pings every
//! keepalive interval.
//!
//! The server of a data or controller connection only answers. A request
//! or notification from it, a binary frame, a frame over the message
//! limit or an unparseable frame ends the connection: every pending call
//! fails with [`ClientError::InvalidMessage`] and the client closes the
//! socket with close code 1002 (1009 for an oversized frame).

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use environment_protocol::{
    error::ErrorData,
    handshake::Plane,
    jsonrpc::{
        self, CancelRequestParams, Id, Message as RpcMessage, Notification, Request, Response,
    },
    limits::MAX_MESSAGE_BYTES,
    manifest::{self, CancelRequest, RequestMethod},
};
use futures_util::{
    SinkExt, StreamExt,
    future::BoxFuture,
    stream::{SplitSink, SplitStream},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
    time::{Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        self, Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

use crate::{
    dial::{self, Dialed},
    error::ClientError,
    options::ConnectOptions,
};

/// Serves one request of an in-process client: the request as the server
/// would receive it, answered with the response it would send. An `Err`
/// fails that one call with the given error, as a test's stand-in for a
/// transport failure; the client stays usable.
pub type InProcessHandler =
    Arc<dyn Fn(Request) -> BoxFuture<'static, Result<Response, ClientError>> + Send + Sync>;

/// How long [`Connection::close`] waits for the close frame to be written.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the reader keeps reading for the server's close reply once
/// the writer has stopped.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

type Reply = Result<Response, ClientError>;

/// Calls waiting for a response, and why the connection ended once it
/// has.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pending: HashMap<i64, oneshot::Sender<Reply>>,
    terminated: Option<ClientError>,
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn register(&self, id: i64) -> Result<oneshot::Receiver<Reply>, ClientError> {
        let mut state = self.state();
        if let Some(error) = &state.terminated {
            return Err(error.clone());
        }
        let (sender, receiver) = oneshot::channel();
        state.pending.insert(id, sender);
        Ok(receiver)
    }

    /// Hand a response to its call; a response nobody waits for any more is
    /// discarded.
    fn resolve(&self, id: i64, response: Response) {
        if let Some(sender) = self.state().pending.remove(&id) {
            let _ = sender.send(Ok(response));
        }
    }

    /// Stop waiting for `id`. True when the call was still unanswered.
    fn forget(&self, id: i64) -> bool {
        self.state().pending.remove(&id).is_some()
    }

    fn has_pending(&self) -> bool {
        !self.state().pending.is_empty()
    }

    /// End the connection with `error`: every pending call and every later
    /// call fails with it. Only the first cause is kept.
    fn terminate(&self, error: ClientError) {
        let mut state = self.state();
        if state.terminated.is_none() {
            state.terminated = Some(error.clone());
        }
        for (_, sender) in state.pending.drain() {
            let _ = sender.send(Err(error.clone()));
        }
    }

    fn terminated(&self) -> Option<ClientError> {
        self.state().terminated.clone()
    }
}

enum Outgoing {
    Text(String),
    /// Send `frame` (if any), then stop writing. `ack` learns whether the
    /// frame was written.
    Close {
        frame: Option<CloseFrame>,
        ack: Option<oneshot::Sender<Result<(), String>>>,
    },
}

enum Backend {
    Socket {
        outgoing: mpsc::UnboundedSender<Outgoing>,
    },
    InProcess {
        handler: InProcessHandler,
    },
}

/// One connection of either plane.
pub(crate) struct Connection {
    plane: Plane,
    shared: Arc<Shared>,
    backend: Backend,
    next_id: AtomicI64,
    request_timeout: Option<Duration>,
}

impl Connection {
    /// Dial `endpoint` (no `initialize` yet).
    pub(crate) async fn connect(
        endpoint: &str,
        options: &ConnectOptions,
        plane: Plane,
    ) -> Result<Self, ClientError> {
        Ok(match dial::dial(endpoint, options).await? {
            Dialed::Tcp(socket) => Self::over_socket(*socket, plane, options),
            Dialed::Unix(socket) => Self::over_socket(*socket, plane, options),
        })
    }

    /// Upgrade an already connected byte stream (no `initialize` yet).
    pub(crate) async fn connect_over<S>(
        stream: S,
        options: &ConnectOptions,
        plane: Plane,
    ) -> Result<Self, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let socket = dial::upgrade(stream, options).await?;
        Ok(Self::over_socket(socket, plane, options))
    }

    fn over_socket<S>(socket: WebSocketStream<S>, plane: Plane, options: &ConnectOptions) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (sink, stream) = socket.split();
        let shared = Arc::new(Shared::default());
        let (outgoing, outgoing_receiver) = mpsc::unbounded_channel();
        let (writer_done, writer_stopped) = oneshot::channel();
        let keepalive = options.keepalive_interval.max(Duration::from_millis(1));
        tokio::spawn(write_loop(
            sink,
            outgoing_receiver,
            shared.clone(),
            keepalive,
            writer_done,
        ));
        tokio::spawn(read_loop(
            stream,
            shared.clone(),
            outgoing.downgrade(),
            writer_stopped,
        ));
        Self {
            plane,
            shared,
            backend: Backend::Socket { outgoing },
            next_id: AtomicI64::new(1),
            request_timeout: options.request_timeout,
        }
    }

    pub(crate) fn in_process(plane: Plane, handler: InProcessHandler) -> Self {
        Self {
            plane,
            shared: Arc::new(Shared::default()),
            backend: Backend::InProcess { handler },
            next_id: AtomicI64::new(1),
            request_timeout: None,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.shared.terminated().is_some()
    }

    /// Close the connection: pending calls fail with `Closed`, a close
    /// frame is written (waiting at most a few seconds), and every later
    /// call fails. Closing again does nothing.
    pub(crate) async fn close(&self) -> Result<(), ClientError> {
        self.shared.terminate(ClientError::Closed {
            code: Some(u16::from(CloseCode::Normal)),
            reason: None,
        });
        let Backend::Socket { outgoing } = &self.backend else {
            return Ok(());
        };
        let (ack, written) = oneshot::channel();
        let close = Outgoing::Close {
            frame: Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }),
            ack: Some(ack),
        };
        if outgoing.send(close).is_err() {
            return Ok(());
        }
        match tokio::time::timeout(CLOSE_TIMEOUT, written).await {
            Ok(Ok(Err(message))) => Err(ClientError::Transport(message)),
            _ => Ok(()),
        }
    }

    /// A typed call of this connection's plane, with the connection's
    /// default deadline unless `timeout` overrides it.
    pub(crate) async fn call<M: RequestMethod>(
        &self,
        params: &M::Params,
        timeout: Option<Duration>,
    ) -> Result<M::Response, ClientError> {
        if M::PLANE != self.plane {
            return Err(ClientError::Encode(format!(
                "{} is a {} method; this client serves the {} plane",
                M::NAME,
                M::PLANE,
                self.plane
            )));
        }
        let response = self
            .request(
                M::NAME,
                params_value(params)?,
                timeout.or(self.request_timeout),
            )
            .await?;
        decode(response)
    }

    /// Run `initialize`: a `versionMismatch` answer, a response naming
    /// another plane or another `protocolVersion` fail with the typed
    /// handshake errors before the response is decoded strictly.
    pub(crate) async fn initialize<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        local_version: u32,
        timeout: Duration,
    ) -> Result<R, ClientError> {
        let response = self
            .request(method, params_value(params)?, Some(timeout))
            .await?;
        let id = response.id.clone();
        let value = match response.result {
            Ok(value) => value,
            Err(error) => {
                return Err(match error.data {
                    ErrorData::VersionMismatch { expected, .. } => ClientError::VersionMismatch {
                        plane: self.plane,
                        local: local_version,
                        remote: expected,
                    },
                    _ => ClientError::Protocol(error),
                });
            }
        };
        if let Some(actual) = value
            .get("plane")
            .and_then(|plane| serde_json::from_value::<Plane>(plane.clone()).ok())
            && actual != self.plane
        {
            return Err(ClientError::WrongPlane {
                expected: self.plane,
                actual,
            });
        }
        if let Some(remote) = value.get("protocolVersion").and_then(Value::as_u64)
            && remote != u64::from(local_version)
        {
            return Err(ClientError::VersionMismatch {
                plane: self.plane,
                local: local_version,
                remote: u32::try_from(remote).unwrap_or(u32::MAX),
            });
        }
        decode(Response {
            id,
            result: Ok(value),
        })
    }

    /// Send one request and wait for its response, at most `timeout`.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Reply {
        let started = Instant::now();
        if let Some(error) = self.shared.terminated() {
            return Err(error);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request {
            id: Id::Number(id),
            method: method.to_owned(),
            params,
        };
        let text = request.to_text();
        check_size(&text)?;
        match &self.backend {
            Backend::Socket { outgoing } => {
                let mut receiver = self.shared.register(id)?;
                let _pending = PendingCall {
                    shared: &self.shared,
                    outgoing,
                    id,
                    cancel: self.plane == Plane::Data
                        && manifest::find(Plane::Data, method).is_some_and(|spec| spec.abortable),
                };
                if outgoing.send(Outgoing::Text(text)).is_err() {
                    return Err(self.ended());
                }
                match timeout {
                    None => receiver.await.unwrap_or_else(|_| Err(self.ended())),
                    Some(timeout) => match tokio::time::timeout(timeout, &mut receiver).await {
                        Ok(reply) => reply.unwrap_or_else(|_| Err(self.ended())),
                        // A response that raced the deadline still counts.
                        Err(_) => receiver.try_recv().unwrap_or_else(|_| {
                            Err(ClientError::DeadlineExceeded {
                                elapsed_ms: elapsed_ms(started),
                            })
                        }),
                    },
                }
            }
            Backend::InProcess { handler } => {
                let call = handler(request);
                let response = match timeout {
                    None => call.await?,
                    Some(timeout) => {
                        tokio::time::timeout(timeout, call).await.map_err(|_| {
                            ClientError::DeadlineExceeded {
                                elapsed_ms: elapsed_ms(started),
                            }
                        })??
                    }
                };
                // The response crosses the same encoding a socket would
                // apply, the server's message limit included.
                match jsonrpc::parse(&response.to_text()) {
                    Ok(RpcMessage::Response(response)) if response.id == Some(Id::Number(id)) => {
                        Ok(response)
                    }
                    Ok(_) | Err(_) => Err(ClientError::InvalidMessage(format!(
                        "the in-process handler did not answer request {id}"
                    ))),
                }
            }
        }
    }

    fn ended(&self) -> ClientError {
        self.shared.terminated().unwrap_or(ClientError::Closed {
            code: None,
            reason: None,
        })
    }
}

/// A registered call. Dropping it (the call finished, ran past its
/// deadline, or its future was dropped) removes the entry; an entry that
/// was still unanswered gets `$/cancelRequest` when the method is
/// abortable.
struct PendingCall<'a> {
    shared: &'a Shared,
    outgoing: &'a mpsc::UnboundedSender<Outgoing>,
    id: i64,
    cancel: bool,
}

impl Drop for PendingCall<'_> {
    fn drop(&mut self) {
        if !self.shared.forget(self.id) || !self.cancel || self.shared.terminated().is_some() {
            return;
        }
        if let Ok(notification) = Notification::new(
            CancelRequest::NAME,
            &CancelRequestParams {
                id: Id::Number(self.id),
            },
        ) {
            let _ = self.outgoing.send(Outgoing::Text(notification.to_text()));
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn check_size(text: &str) -> Result<(), ClientError> {
    let actual_bytes = text.len() as u64;
    if actual_bytes > MAX_MESSAGE_BYTES {
        return Err(ClientError::PayloadTooLarge {
            limit_bytes: MAX_MESSAGE_BYTES,
            actual_bytes,
        });
    }
    Ok(())
}

fn params_value<P: Serialize>(params: &P) -> Result<Value, ClientError> {
    match serde_json::to_value(params) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err(ClientError::Encode(
            "params must encode as a JSON object".to_owned(),
        )),
        Err(error) => Err(ClientError::Encode(error.to_string())),
    }
}

fn decode<T: DeserializeOwned>(response: Response) -> Result<T, ClientError> {
    response.decode().map_err(|error| match error {
        jsonrpc::ResponseError::Failed(error) => ClientError::Protocol(error),
        jsonrpc::ResponseError::Undecodable(message) => ClientError::Decode(message),
    })
}

async fn write_loop<S>(
    mut sink: SplitSink<WebSocketStream<S>, Message>,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
    shared: Arc<Shared>,
    keepalive: Duration,
    // Dropped when the writer stops, which starts the reader's close grace.
    _done: oneshot::Sender<()>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ticker = tokio::time::interval_at(Instant::now() + keepalive, keepalive);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = outgoing.recv() => match command {
                Some(Outgoing::Text(text)) => {
                    if let Err(error) = sink.send(Message::text(text)).await {
                        shared.terminate(ClientError::Transport(error.to_string()));
                        return;
                    }
                }
                Some(Outgoing::Close { frame, ack }) => {
                    let written = match frame {
                        Some(frame) => sink
                            .send(Message::Close(Some(frame)))
                            .await
                            .map_err(|error| error.to_string()),
                        None => Ok(()),
                    };
                    if let Some(ack) = ack {
                        let _ = ack.send(written);
                    }
                    return;
                }
                // Every client handle is gone.
                None => {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Normal,
                            reason: "".into(),
                        })))
                        .await;
                    return;
                }
            },
            _ = ticker.tick() => {
                if shared.has_pending()
                    && let Err(error) = sink.send(Message::Ping(Default::default())).await
                {
                    shared.terminate(ClientError::Transport(error.to_string()));
                    return;
                }
            }
        }
    }
}

async fn read_loop<S>(
    mut stream: SplitStream<WebSocketStream<S>>,
    shared: Arc<Shared>,
    outgoing: mpsc::WeakUnboundedSender<Outgoing>,
    mut writer_stopped: oneshot::Receiver<()>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut grace_until: Option<Instant> = None;
    let violation = loop {
        let frame = tokio::select! {
            frame = stream.next() => frame,
            _ = &mut writer_stopped, if grace_until.is_none() => {
                grace_until = Some(Instant::now() + CLOSE_GRACE);
                continue;
            }
            _ = tokio::time::sleep_until(grace_until.unwrap_or_else(Instant::now)),
                if grace_until.is_some() => break None,
        };
        match frame {
            None => break None,
            Some(Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)) => {
                break None;
            }
            Some(Err(tungstenite::Error::Capacity(error))) => {
                break Some((
                    CloseCode::Size,
                    format!("frame over the {MAX_MESSAGE_BYTES}-byte message limit: {error}"),
                ));
            }
            Some(Err(error)) => {
                shared.terminate(ClientError::Transport(error.to_string()));
                break None;
            }
            Some(Ok(Message::Text(text))) => match jsonrpc::parse(&text) {
                Ok(RpcMessage::Response(response)) => match &response.id {
                    Some(Id::Number(id)) => shared.resolve(*id, response),
                    // This client never sends string ids: nothing waits.
                    Some(Id::String(_)) => {}
                    None => {
                        let error = response
                            .result
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_default();
                        break Some((
                            CloseCode::Protocol,
                            format!("the server answered with a null id: {error}"),
                        ));
                    }
                },
                Ok(RpcMessage::Request(request)) => {
                    break Some((
                        CloseCode::Protocol,
                        format!("the server sent a request ({})", request.method),
                    ));
                }
                Ok(RpcMessage::Notification(notification)) => {
                    break Some((
                        CloseCode::Protocol,
                        format!("the server sent a notification ({})", notification.method),
                    ));
                }
                Err(rejection) => {
                    break Some((
                        CloseCode::Protocol,
                        format!("unacceptable frame: {}", rejection.error),
                    ));
                }
            },
            Some(Ok(Message::Binary(_))) => {
                break Some((
                    CloseCode::Protocol,
                    "binary frames are not part of the protocol".to_owned(),
                ));
            }
            Some(Ok(Message::Close(frame))) => {
                // Keep reading: the next read flushes the close reply and
                // then ends the stream.
                shared.terminate(ClientError::Closed {
                    code: frame.as_ref().map(|frame| u16::from(frame.code)),
                    reason: frame
                        .map(|frame| frame.reason.to_string())
                        .filter(|reason| !reason.is_empty()),
                });
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
        }
    };
    let frame = match violation {
        Some((code, message)) => {
            shared.terminate(ClientError::InvalidMessage(message.clone()));
            let mut reason = message;
            truncate_reason(&mut reason);
            Some(CloseFrame {
                code,
                reason: reason.into(),
            })
        }
        None => {
            shared.terminate(ClientError::Closed {
                code: None,
                reason: None,
            });
            None
        }
    };
    if let Some(outgoing) = outgoing.upgrade() {
        let _ = outgoing.send(Outgoing::Close { frame, ack: None });
    }
}

/// A close reason fits in a control frame: at most 123 bytes.
fn truncate_reason(reason: &mut String) {
    const MAX: usize = 123;
    if reason.len() > MAX {
        let mut end = MAX;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use environment_protocol::{
        data::idle::{GetIdleParams, GetIdleResponse},
        error::ProtocolError,
        manifest::{EnvGetIdle, TargetGet},
    };

    use super::*;

    fn idle_handler(response: Value) -> InProcessHandler {
        Arc::new(move |request: Request| {
            let response = response.clone();
            Box::pin(async move { Ok(Response::success(request.id, &response)) })
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_process_calls_decode_strictly() {
        let connection = Connection::in_process(
            Plane::Data,
            idle_handler(serde_json::json!({"idleForMs": 5})),
        );
        let idle = connection
            .call::<EnvGetIdle>(&GetIdleParams {}, None)
            .await
            .unwrap();
        assert_eq!(
            idle,
            GetIdleResponse {
                idle_for_ms: 5,
                ..GetIdleResponse::default()
            }
        );
        let broken = Connection::in_process(
            Plane::Data,
            idle_handler(serde_json::json!({"idleForMs": "soon"})),
        );
        assert!(matches!(
            broken.call::<EnvGetIdle>(&GetIdleParams {}, None).await,
            Err(ClientError::Decode(_))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_errors_keep_their_kind() {
        let connection = Connection::in_process(
            Plane::Data,
            Arc::new(|request: Request| {
                Box::pin(async move {
                    Ok(Response::failure(
                        Some(request.id),
                        ProtocolError::not_initialized(),
                    ))
                })
            }),
        );
        let error = connection
            .call::<EnvGetIdle>(&GetIdleParams {}, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.protocol_kind(),
            Some(environment_protocol::error::ProtocolErrorKind::NotInitialized)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn methods_of_another_plane_are_refused_before_sending() {
        let connection = Connection::in_process(Plane::Data, idle_handler(serde_json::json!({})));
        let params = serde_json::from_value(serde_json::json!({
            "target": {"owner": {"universeId": "u", "environmentId": "e"}}
        }))
        .unwrap();
        assert!(matches!(
            connection.call::<TargetGet>(&params, None).await,
            Err(ClientError::Encode(_))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_connections_refuse_calls() {
        let connection = Connection::in_process(
            Plane::Data,
            idle_handler(serde_json::json!({"idleForMs": 0})),
        );
        connection.close().await.unwrap();
        assert!(connection.is_closed());
        assert!(matches!(
            connection.call::<EnvGetIdle>(&GetIdleParams {}, None).await,
            Err(ClientError::Closed {
                code: Some(1000),
                ..
            })
        ));
    }

    #[test]
    fn oversized_requests_are_refused_with_their_size() {
        let text = "x".repeat(MAX_MESSAGE_BYTES as usize + 1);
        assert!(matches!(
            check_size(&text),
            Err(ClientError::PayloadTooLarge { limit_bytes: MAX_MESSAGE_BYTES, actual_bytes })
                if actual_bytes == MAX_MESSAGE_BYTES + 1
        ));
    }
}
