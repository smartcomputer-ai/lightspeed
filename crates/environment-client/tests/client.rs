//! Socket behaviour of the data and controller clients against scripted
//! WebSocket servers.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use environment_client::{
    ClientError, ConnectOptions, ControllerClient, DataClient,
    conformance::controller::{ControllerConformance, run},
};
use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION, ENVD_PROTOCOL_VERSION,
    bytes::ByteChunk,
    control::{handshake::ControllerInitializeResponse, target::TargetOwner},
    data::{
        exec::ReturnOn,
        fs::{WriteCondition, WriteFileParams},
        handshake::InitializeResponse,
        idle::{GetIdleParams, GetIdleResponse},
        process::ReadProcessParams,
    },
    error::{ProtocolError, ProtocolErrorKind},
    handshake::{ImplementationInfo, Plane},
    ids::{EnvironmentId, ProcessId, TemplateId, UniverseId},
    jsonrpc::{self, CancelRequestParams, Id, Message as RpcMessage, Request, Response},
    limits::{MAX_MESSAGE_BYTES, RETRY_AFTER_MAX_SECS},
    manifest::{CancelRequest, EnvGetIdle},
    paths::EnvironmentPath,
    routes::DataRouteRefusal,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, UnixListener},
    sync::mpsc,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{
            ErrorResponse, Request as UpgradeRequest, Response as UpgradeResponse,
        },
        http::StatusCode,
    },
};

fn fixture(path: &str) -> Value {
    let text = match path {
        "data" => include_str!("../../environment-protocol/fixtures/data/initialize.json"),
        "controller" => {
            include_str!("../../environment-protocol/fixtures/controller/initialize.json")
        }
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str::<Value>(text).expect("fixture")["response"].clone()
}

fn options() -> ConnectOptions {
    ConnectOptions::new(
        ImplementationInfo::new("lightspeed-client-test")
            .and_then(|info| info.with_version("0.0.0"))
            .expect("client info"),
    )
}

/// Serve one WebSocket connection on a fresh loopback port with `serve`.
async fn server<F, Fut>(serve: F) -> String
where
    F: FnOnce(WebSocketStream<tokio::net::TcpStream>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let url = format!("ws://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        serve(accept_async(stream).await.expect("upgrade")).await;
    });
    url
}

/// The next frame that is a request, skipping pings.
async fn next_request<S>(socket: &mut WebSocketStream<S>) -> Option<Request>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match socket.next().await?.ok()? {
            Message::Text(text) => match jsonrpc::parse(&text).expect("valid frame") {
                RpcMessage::Request(request) => return Some(request),
                RpcMessage::Notification(_) | RpcMessage::Response(_) => {}
            },
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

async fn send<S>(socket: &mut WebSocketStream<S>, response: Response)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    socket
        .send(Message::text(response.to_text()))
        .await
        .expect("send response");
}

/// Answer the first request, `initialize`, with `result`.
async fn answer_initialize<S>(socket: &mut WebSocketStream<S>, result: Value)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = next_request(socket).await.expect("initialize");
    assert_eq!(request.method, "initialize");
    assert_eq!(
        request.params["protocolVersion"],
        json!(ENVD_PROTOCOL_VERSION)
    );
    send(socket, Response::success(request.id, &result)).await;
}

fn idle(ms: u64) -> GetIdleResponse {
    GetIdleResponse {
        idle_for_ms: ms,
        ..GetIdleResponse::default()
    }
}

fn read_params() -> ReadProcessParams {
    ReadProcessParams {
        process_id: ProcessId::new("p1").expect("process id"),
        after: None,
        tail_bytes: None,
        ack: None,
        max_bytes: 1024,
        wait_ms: 60_000,
        return_on: ReturnOn::AnyOutput,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn connect_initialized_returns_the_handshake_and_close_sends_a_close_frame() {
    let (saw_close, mut closed) = mpsc::unbounded_channel();
    let url = server(move |mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                let _ = saw_close.send(());
                return;
            }
        }
    })
    .await;
    let (client, initialize) = DataClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    let expected: InitializeResponse = serde_json::from_value(fixture("data")).expect("fixture");
    assert_eq!(initialize, expected);
    assert_eq!(client.initialize_response(), &expected);
    client.close().await.expect("close");
    assert!(client.is_closed());
    tokio::time::timeout(Duration::from_secs(5), closed.recv())
        .await
        .expect("server saw the close frame in time")
        .expect("close frame");
    assert!(matches!(
        client.get_idle(&GetIdleParams {}).await,
        Err(ClientError::Closed { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_version_mismatch_answer_is_typed() {
    let url = server(|mut socket| async move {
        let request = next_request(&mut socket).await.expect("initialize");
        let received = request.params["protocolVersion"].as_u64().unwrap() as u32;
        send(
            &mut socket,
            Response::failure(
                Some(request.id),
                ProtocolError::version_mismatch(2, received),
            ),
        )
        .await;
        while socket.next().await.is_some() {}
    })
    .await;
    let error = DataClient::connect_initialized(&url, options())
        .await
        .expect_err("mismatch");
    assert!(
        matches!(
            error,
            ClientError::VersionMismatch {
                plane: Plane::Data,
                local: ENVD_PROTOCOL_VERSION,
                remote: 2
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        error.protocol_kind(),
        Some(ProtocolErrorKind::VersionMismatch)
    );
    assert!(!error.is_retryable());
}

#[tokio::test(flavor = "current_thread")]
async fn a_response_with_another_version_is_a_mismatch() {
    let mut response = fixture("data");
    response["protocolVersion"] = json!(ENVD_PROTOCOL_VERSION + 1);
    let url = server(move |mut socket| async move {
        answer_initialize(&mut socket, response).await;
        while socket.next().await.is_some() {}
    })
    .await;
    assert!(matches!(
        DataClient::connect_initialized(&url, options()).await,
        Err(ClientError::VersionMismatch { remote, .. }) if remote == ENVD_PROTOCOL_VERSION + 1
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_controller_answering_a_data_client_is_the_wrong_plane() {
    let url = server(|mut socket| async move {
        answer_initialize(&mut socket, fixture("controller")).await;
        while socket.next().await.is_some() {}
    })
    .await;
    assert!(matches!(
        DataClient::connect_initialized(&url, options()).await,
        Err(ClientError::WrongPlane {
            expected: Plane::Data,
            actual: Plane::Controller
        })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn controller_clients_check_their_own_plane_and_version() {
    let url = server(|mut socket| async move {
        let request = next_request(&mut socket).await.expect("initialize");
        assert_eq!(
            request.params["protocolVersion"],
            json!(CONTROLLER_PROTOCOL_VERSION)
        );
        send(
            &mut socket,
            Response::success(request.id, &fixture("controller")),
        )
        .await;
        while socket.next().await.is_some() {}
    })
    .await;
    let (client, initialize) = ControllerClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    let expected: ControllerInitializeResponse =
        serde_json::from_value(fixture("controller")).expect("fixture");
    assert_eq!(initialize, expected);
    assert!(initialize.capabilities.ingress);
    client.close().await.expect("close");
}

#[tokio::test(flavor = "current_thread")]
async fn pipelined_calls_are_matched_by_id() {
    let url = server(|mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        let first = next_request(&mut socket).await.expect("first");
        let second = next_request(&mut socket).await.expect("second");
        // Answer in reverse order; each result names its own id.
        for request in [second, first] {
            let Id::Number(id) = request.id else {
                panic!("numeric ids")
            };
            send(&mut socket, Response::success(request.id, &idle(id as u64))).await;
        }
        while socket.next().await.is_some() {}
    })
    .await;
    let (client, _) = DataClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    let (first, second) = tokio::join!(
        client.call::<EnvGetIdle>(&GetIdleParams {}),
        client.get_idle(&GetIdleParams {}),
    );
    let (first, second) = (first.expect("first"), second.expect("second"));
    assert_ne!(first.idle_for_ms, second.idle_for_ms);
    assert!(
        first.idle_for_ms < second.idle_for_ms,
        "first sent, first id"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_notification_fails_pending_calls_and_closes() {
    let url = server(|mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        let _ = next_request(&mut socket).await.expect("request");
        socket
            .send(Message::text(
                r#"{"jsonrpc":"2.0","method":"process/output","params":{}}"#,
            ))
            .await
            .expect("send notification");
        while socket.next().await.is_some() {}
    })
    .await;
    let (client, _) = DataClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    assert!(matches!(
        client.get_idle(&GetIdleParams {}).await,
        Err(ClientError::InvalidMessage(_))
    ));
    assert!(client.is_closed());
    assert!(matches!(
        client.get_idle(&GetIdleParams {}).await,
        Err(ClientError::InvalidMessage(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_binary_frame_is_a_protocol_violation() {
    let url = server(|mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        let _ = next_request(&mut socket).await.expect("request");
        socket
            .send(Message::binary(b"{}".to_vec()))
            .await
            .expect("send binary");
        while socket.next().await.is_some() {}
    })
    .await;
    let (client, _) = DataClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    assert!(matches!(
        client.get_idle(&GetIdleParams {}).await,
        Err(ClientError::InvalidMessage(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_deadline_cancels_an_abortable_call() {
    let (cancelled, mut cancels) = mpsc::unbounded_channel();
    let url = server(move |mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        let read = next_request(&mut socket).await.expect("process/read");
        assert_eq!(read.method, "process/read");
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message
                && let Ok(RpcMessage::Notification(notification)) = jsonrpc::parse(&text)
            {
                assert_eq!(notification.method, CancelRequest::NAME);
                let params: CancelRequestParams = notification.params().expect("params");
                let _ = cancelled.send((params.id, read.id.clone()));
                // The late answer is discarded by the client.
                send(
                    &mut socket,
                    Response::failure(Some(read.id.clone()), ProtocolError::cancelled()),
                )
                .await;
            }
        }
    })
    .await;
    let (client, _) = DataClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    let error = client
        .call_with_timeout::<environment_protocol::manifest::ProcessRead>(
            &read_params(),
            Duration::from_millis(50),
        )
        .await
        .expect_err("deadline");
    assert!(
        matches!(error, ClientError::DeadlineExceeded { .. }),
        "{error:?}"
    );
    let (cancelled_id, read_id) = tokio::time::timeout(Duration::from_secs(5), cancels.recv())
        .await
        .expect("cancel in time")
        .expect("cancel");
    assert_eq!(cancelled_id, read_id);
    assert!(
        !client.is_closed(),
        "a deadline leaves the connection usable"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_dropped_abortable_call_is_cancelled() {
    let (cancelled, mut cancels) = mpsc::unbounded_channel();
    let url = server(move |mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        let read = next_request(&mut socket).await.expect("process/read");
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message
                && let Ok(RpcMessage::Notification(notification)) = jsonrpc::parse(&text)
            {
                let params: CancelRequestParams = notification.params().expect("params");
                let _ = cancelled.send(params.id == read.id);
            }
        }
    })
    .await;
    let (client, _) = DataClient::connect_initialized(&url, options())
        .await
        .expect("connect");
    let params = read_params();
    let read = client.read_process(&params);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), read)
            .await
            .is_err(),
        "the read waits"
    );
    let matched = tokio::time::timeout(Duration::from_secs(5), cancels.recv())
        .await
        .expect("cancel in time")
        .expect("cancel");
    assert!(matched, "the cancel names the dropped request");
}

#[tokio::test(flavor = "current_thread")]
async fn pending_calls_keep_the_connection_alive_with_pings() {
    let pings = Arc::new(AtomicUsize::new(0));
    let counted = pings.clone();
    let url = server(move |mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        let request = next_request(&mut socket).await.expect("request");
        let quiet = tokio::time::sleep(Duration::from_millis(300));
        tokio::pin!(quiet);
        loop {
            tokio::select! {
                _ = &mut quiet => break,
                message = socket.next() => match message {
                    Some(Ok(Message::Ping(_))) => { counted.fetch_add(1, Ordering::SeqCst); }
                    Some(Ok(_)) => {}
                    _ => return,
                },
            }
        }
        send(&mut socket, Response::success(request.id, &idle(0))).await;
        while socket.next().await.is_some() {}
    })
    .await;
    let mut options = options();
    options.keepalive_interval = Duration::from_millis(40);
    let (client, _) = DataClient::connect_initialized(&url, options)
        .await
        .expect("connect");
    client.get_idle(&GetIdleParams {}).await.expect("idle");
    assert!(
        pings.load(Ordering::SeqCst) >= 3,
        "pinged {} times",
        pings.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_default_request_timeout_applies_to_every_call() {
    let url = server(|mut socket| async move {
        answer_initialize(&mut socket, fixture("data")).await;
        while socket.next().await.is_some() {}
    })
    .await;
    let (client, _) = DataClient::connect_initialized(
        &url,
        options().with_request_timeout(Duration::from_millis(50)),
    )
    .await
    .expect("connect");
    assert!(matches!(
        client.get_idle(&GetIdleParams {}).await,
        Err(ClientError::DeadlineExceeded { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_requests_fail_without_being_sent() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let client = DataClient::in_process(
        serde_json::from_value(fixture("data")).expect("fixture"),
        move |request: Request| {
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(Response::success(request.id, &json!({}))) })
        },
    );
    let error = client
        .write_file(&WriteFileParams {
            path: EnvironmentPath::new("/workspace/big").expect("path"),
            data: ByteChunk::new(vec![0_u8; MAX_MESSAGE_BYTES as usize]),
            create: WriteCondition::default(),
            if_match: None,
            executable: false,
        })
        .await
        .expect_err("too large");
    assert!(matches!(
        error,
        ClientError::PayloadTooLarge { limit_bytes: MAX_MESSAGE_BYTES, actual_bytes }
            if actual_bytes > MAX_MESSAGE_BYTES
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn in_process_clients_answer_through_the_handler() {
    let client = DataClient::in_process(
        serde_json::from_value(fixture("data")).expect("fixture"),
        |request: Request| {
            Box::pin(async move {
                assert_eq!(request.method, "env/getIdle");
                Ok(Response::success(request.id, &idle(7)))
            })
        },
    );
    assert_eq!(
        client.get_idle(&GetIdleParams {}).await.expect("idle"),
        idle(7)
    );
}

#[allow(clippy::result_large_err)]
fn refuse_upgrade(
    _: &UpgradeRequest,
    _: UpgradeResponse,
) -> Result<UpgradeResponse, ErrorResponse> {
    let mut response = ErrorResponse::new(None);
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    Err(response)
}

#[tokio::test(flavor = "current_thread")]
async fn an_upgrade_refusal_reports_its_status() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let url = format!("ws://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let _ = accept_hdr_async(stream, refuse_upgrade).await;
    });
    let error = DataClient::connect_initialized(&url, options())
        .await
        .expect_err("refused");
    assert!(
        matches!(
            error,
            ClientError::UpgradeRejected {
                status: 401,
                refusal: None,
                retry_after_ms: None,
            }
        ),
        "{error:?}"
    );
    assert!(!error.is_retryable());
}

/// Serve one data route refusal: `status`, `Retry-After` when given, and a
/// JSON body.
#[allow(clippy::result_large_err)]
async fn refusing_route(
    status: StatusCode,
    retry_after: Option<&'static str>,
    body: Value,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let url = format!("ws://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let _ = accept_hdr_async(stream, move |_: &UpgradeRequest, _: UpgradeResponse| {
            let mut response = ErrorResponse::new(Some(body.to_string()));
            *response.status_mut() = status;
            let headers = response.headers_mut();
            headers.insert("content-type", "application/json".parse().unwrap());
            if let Some(retry_after) = retry_after {
                headers.insert("retry-after", retry_after.parse().unwrap());
            }
            Err(response)
        })
        .await;
    });
    url
}

#[tokio::test(flavor = "current_thread")]
async fn a_data_route_refusal_is_typed_with_its_retry_after() {
    let url = refusing_route(
        StatusCode::SERVICE_UNAVAILABLE,
        Some("3"),
        json!({"state": "waking"}),
    )
    .await;
    let error = DataClient::connect_initialized(&url, options())
        .await
        .expect_err("refused");
    assert!(
        matches!(
            &error,
            ClientError::UpgradeRejected {
                status: 503,
                refusal: Some(DataRouteRefusal::Waking {}),
                retry_after_ms: Some(3_000),
            }
        ),
        "{error:?}"
    );
    assert!(error.is_retryable());
    assert_eq!(error.to_string(), "upgrade refused with HTTP 503: waking");

    let url = refusing_route(
        StatusCode::SERVICE_UNAVAILABLE,
        Some("86400"),
        json!({"state": "booting"}),
    )
    .await;
    let error = DataClient::connect_initialized(&url, options())
        .await
        .expect_err("refused");
    assert_eq!(error.retry_after_ms(), Some(RETRY_AFTER_MAX_SECS * 1_000));

    let url = refusing_route(StatusCode::GONE, None, json!({"state": "gone"})).await;
    let error = DataClient::connect_initialized(&url, options())
        .await
        .expect_err("refused");
    assert_eq!(error.data_route_refusal(), Some(&DataRouteRefusal::Gone {}));
    assert!(!error.is_retryable());
    assert_eq!(
        environment_client::conformance::data_route::check_refusal(&error),
        Ok(&DataRouteRefusal::Gone {})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_undecodable_refusal_body_keeps_the_status() {
    let url = refusing_route(
        StatusCode::SERVICE_UNAVAILABLE,
        None,
        json!({"state": "sleeping"}),
    )
    .await;
    let error = DataClient::connect_initialized(&url, options())
        .await
        .expect_err("refused");
    assert!(
        matches!(
            error,
            ClientError::UpgradeRejected {
                status: 503,
                refusal: None,
                retry_after_ms: None,
            }
        ),
        "{error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_silent_server_hits_the_connect_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let url = format!("ws://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let mut options = options();
    options.connect_timeout = Duration::from_millis(100);
    assert!(matches!(
        DataClient::connect_initialized(&url, options).await,
        Err(ClientError::ConnectTimeout { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn unix_endpoints_upgrade_over_the_socket() {
    let path = std::path::PathBuf::from(format!(
        "/tmp/lightspeed-client-{}-{}.sock",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind unix socket");
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = accept_async(stream).await.expect("upgrade");
        answer_initialize(&mut socket, fixture("data")).await;
        while socket.next().await.is_some() {}
    });
    let endpoint = format!("unix://{}", path.display());
    let (client, _) = DataClient::connect_initialized(&endpoint, options())
        .await
        .expect("connect over unix");
    client.close().await.expect("close");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "current_thread")]
async fn clients_connect_over_an_existing_byte_stream() {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut socket = accept_async(server_end).await.expect("upgrade");
        answer_initialize(&mut socket, fixture("data")).await;
        let request = next_request(&mut socket).await.expect("request");
        send(&mut socket, Response::success(request.id, &idle(3))).await;
        while socket.next().await.is_some() {}
    });
    let (client, _) = DataClient::connect_initialized_over(client_end, options())
        .await
        .expect("connect over duplex");
    assert_eq!(
        client.get_idle(&GetIdleParams {}).await.expect("idle"),
        idle(3)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn conformance_reports_an_unreachable_controller() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let url = format!("ws://{}", listener.local_addr().expect("address"));
    drop(listener);
    let owner = Arc::new(|| TargetOwner {
        universe_id: UniverseId::new("u1").unwrap(),
        environment_id: EnvironmentId::new("e1").unwrap(),
    });
    let report = run(&ControllerConformance::new(
        url,
        options().with_bearer_token("credential"),
        TemplateId::new("t1").unwrap(),
        owner,
    ))
    .await;
    assert!(!report.passed());
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, "initialize");
    assert!(report.into_result().is_err());
}
