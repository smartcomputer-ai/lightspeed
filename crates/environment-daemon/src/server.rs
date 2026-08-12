use std::time::Duration;

use anyhow::{Context as _, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt as _, StreamExt as _};
use host_protocol::{
    data::{
        fs::{
            CopyParams, CreateDirectoryParams, GetMetadataParams, GlobFilesParams,
            ReadDirectoryParams, ReadFileParams, RemoveParams, SearchTextParams, WriteFileParams,
        },
        handshake::{InitializeParams, InitializeResponse, InitializedParams},
        jobs::{CancelJobsParams, ListJobsParams, ReadJobsParams, StartJobsParams},
        methods::{
            FS_COPY_METHOD, FS_CREATE_DIRECTORY_METHOD, FS_GET_METADATA_METHOD,
            FS_GLOB_FILES_METHOD, FS_READ_DIRECTORY_METHOD, FS_READ_FILE_METHOD, FS_REMOVE_METHOD,
            FS_SEARCH_TEXT_METHOD, FS_WRITE_FILE_METHOD,
            INITIALIZE_METHOD as DATA_INITIALIZE_METHOD, INITIALIZED_METHOD, JOB_CANCEL_METHOD,
            JOB_LIST_METHOD, JOB_READ_METHOD, JOB_START_METHOD, PROCESS_READ_METHOD,
            PROCESS_RESIZE_METHOD, PROCESS_START_METHOD, PROCESS_TERMINATE_METHOD,
            PROCESS_WRITE_METHOD,
        },
        process::{
            ReadProcessParams, ResizeProcessParams, StartProcessParams, TerminateProcessParams,
            WriteProcessParams,
        },
    },
    error::{HostError, HostErrorCode},
    gateway::{
        DIRECT_DAEMON_PATH, DirectDaemonAuthenticate, GatewayAccepted, GatewayChallenge,
        GatewayRejected, direct_auth_message,
    },
    shared::CURRENT_PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc};
use tokio_tungstenite::{
    accept_hdr_async, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

async fn run_provider_listener(runtime: DaemonRuntime) -> anyhow::Result<()> {
    let DaemonMode::Provider { listen, token } = &runtime.config().mode else {
        bail!("not provider mode")
    };
    let listener = TcpListener::bind(listen)
        .await
        .context("bind provider listener")?;
    tracing_line(&format!("provider listener ready on {listen}"));
    loop {
        let (stream, peer) = listener.accept().await?;
        let runtime = runtime.clone();
        let expected = token.clone();
        tokio::spawn(async move {
            let authorized = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let authorized_for_handshake = authorized.clone();
            let websocket = accept_hdr_async(stream, move |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                let valid = request.headers().get(http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()));
                authorized_for_handshake.store(valid, std::sync::atomic::Ordering::Relaxed);
                if valid { Ok(response) } else {
                    Err(http::Response::builder().status(http::StatusCode::UNAUTHORIZED).body(Some("unauthorized".to_owned())).expect("response"))
                }
            }).await;
            match websocket {
                Ok(socket) if authorized.load(std::sync::atomic::Ordering::Relaxed) => {
                    if let Err(error) = run_provider_data_connection(&runtime, socket).await {
                        eprintln!("lightspeed-envd provider connection {peer} failed: {error:#}");
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("lightspeed-envd rejected provider connection {peer}: {error}")
                }
            }
        });
    }
}

async fn run_provider_data_connection<S>(
    runtime: &DaemonRuntime,
    socket: tokio_tungstenite::WebSocketStream<S>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut writer, mut reader) = socket.split();
    let (responses, mut response_rx) = mpsc::channel::<Value>(64);
    let concurrency = std::sync::Arc::new(Semaphore::new(32));
    loop {
        tokio::select! {
            response = response_rx.recv() => {
                let Some(response) = response else { return Ok(()) };
                writer.send(Message::Text(response.to_string().into())).await?;
            }
            message = reader.next() => {
                let Some(message) = message else { return Ok(()) };
                match message? {
                    Message::Text(text) => dispatch_gateway_json(runtime, &responses, &concurrency, &text).await?,
                    Message::Binary(bytes) => {
                        let text = std::str::from_utf8(&bytes).context("binary provider frame is not UTF-8")?;
                        dispatch_gateway_json(runtime, &responses, &concurrency, text).await?;
                    }
                    Message::Ping(bytes) => writer.send(Message::Pong(bytes)).await?,
                    Message::Pong(_) | Message::Frame(_) => {}
                    Message::Close(_) => return Ok(()),
                }
            }
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

use crate::{
    DaemonRuntime,
    config::DaemonMode,
    identity::DaemonIdentity,
    rpc::{
        decode_params, encode_result, error_response, invalid_request, method_not_found,
        parse_request, success_response,
    },
};

pub async fn run(runtime: DaemonRuntime) -> anyhow::Result<()> {
    let DaemonMode::Direct {
        identity: binding,
        enrollment_token: configured_token,
    } = &runtime.config().mode
    else {
        return run_provider_listener(runtime).await;
    };
    let identity = DaemonIdentity::load_or_create(&runtime.config().state_dir, binding).await?;
    let mut enrollment_token = configured_token.clone();
    loop {
        match run_connection(&runtime, &identity, enrollment_token.as_deref()).await {
            Ok(connected) => {
                if connected {
                    enrollment_token = None;
                }
                tracing_line("gateway connection closed");
            }
            Err(error) => eprintln!("lightspeed-envd gateway connection failed: {error:#}"),
        }
        tokio::time::sleep(Duration::from_millis(runtime.config().reconnect_ms)).await;
    }
}

async fn run_connection(
    runtime: &DaemonRuntime,
    identity: &DaemonIdentity,
    enrollment_token: Option<&str>,
) -> anyhow::Result<bool> {
    let DaemonMode::Direct {
        identity: binding, ..
    } = &runtime.config().mode
    else {
        bail!("not a direct daemon")
    };
    let endpoint = format!(
        "{}{DIRECT_DAEMON_PATH}",
        websocket_base(&binding.gateway_url)?
    );
    let request = endpoint.into_client_request()?;
    let (mut socket, _) = connect_async(request)
        .await
        .context("connect environment gateway")?;
    let challenge: GatewayChallenge = recv_json(&mut socket).await?;
    if challenge.protocol_version != CURRENT_PROTOCOL_VERSION {
        bail!(
            "gateway protocol version {} is incompatible with {}",
            challenge.protocol_version,
            CURRENT_PROTOCOL_VERSION
        );
    }
    let daemon_id = identity.daemon_id();
    let signature = identity.sign(&direct_auth_message(
        &challenge.nonce,
        &binding.universe_id,
        &binding.environment_id,
        &binding.incarnation_id,
        &daemon_id,
    ));
    send_json(
        &mut socket,
        &DirectDaemonAuthenticate {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            universe_id: binding.universe_id.clone(),
            environment_id: binding.environment_id.clone(),
            incarnation_id: binding.incarnation_id.clone(),
            daemon_id,
            public_key: URL_SAFE_NO_PAD.encode(identity.public_key()),
            signature: URL_SAFE_NO_PAD.encode(signature),
            enrollment_token: enrollment_token.map(ToOwned::to_owned),
            capabilities: runtime.capabilities(),
            default_cwd: Some(runtime.config().cwd.to_string_lossy().into_owned()),
        },
    )
    .await?;
    let accepted_value: Value = recv_json(&mut socket).await?;
    if let Ok(rejected) = serde_json::from_value::<GatewayRejected>(accepted_value.clone()) {
        bail!("gateway rejected daemon: {}", rejected.message);
    }
    let accepted: GatewayAccepted = serde_json::from_value(accepted_value)?;
    tracing_line(&format!(
        "connected connection_id={} lease_ms={}",
        accepted.connection_id, accepted.lease_ms
    ));

    let (mut writer, mut reader) = socket.split();
    let (responses, mut response_rx) = mpsc::channel::<Value>(64);
    let concurrency = std::sync::Arc::new(Semaphore::new(32));

    loop {
        let timeout = Duration::from_millis(accepted.lease_ms.max(1));
        tokio::select! {
            response = response_rx.recv() => {
                let Some(response) = response else { return Ok(true) };
                writer.send(Message::Text(response.to_string().into())).await?;
            }
            message = tokio::time::timeout(timeout, reader.next()) => {
                let Some(message) = message.context("gateway lease expired")? else {
                    return Ok(true);
                };
                match message? {
                    Message::Text(text) => dispatch_gateway_json(runtime, &responses, &concurrency, &text).await?,
                    Message::Binary(bytes) => {
                        let text = std::str::from_utf8(&bytes).context("binary gateway frame is not UTF-8")?;
                        dispatch_gateway_json(runtime, &responses, &concurrency, text).await?;
                    }
                    Message::Ping(bytes) => writer.send(Message::Pong(bytes)).await?,
                    Message::Pong(_) | Message::Frame(_) => {}
                    Message::Close(_) => return Ok(true),
                }
            }
        }
    }
}

async fn dispatch_gateway_json(
    runtime: &DaemonRuntime,
    responses: &mpsc::Sender<Value>,
    concurrency: &std::sync::Arc<Semaphore>,
    text: &str,
) -> anyhow::Result<()> {
    let value: Value = serde_json::from_str(text)?;
    let request = match parse_request(value) {
        Ok(request) => request,
        Err(error) => {
            responses.send(error_response(None, error)).await?;
            return Ok(());
        }
    };
    let Some(id) = request.id else {
        if request.method.as_deref() == Some(INITIALIZED_METHOD) {
            let _ = decode_params::<InitializedParams>(request.params);
        }
        return Ok(());
    };
    let Ok(permit) = concurrency.clone().try_acquire_owned() else {
        responses
            .send(error_response(
                Some(id),
                HostError::new(HostErrorCode::Conflict, "daemon request limit reached"),
            ))
            .await?;
        return Ok(());
    };
    let runtime = runtime.clone();
    let responses = responses.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let result = match request.method.as_deref() {
            Some(method) => handle_data(&runtime, method, request.params).await,
            None => Err(invalid_request("request missing method")),
        };
        let response = match result {
            Ok(result) => success_response(id, result),
            Err(error) => error_response(Some(id), error),
        };
        let _ = responses.send(response).await;
    });
    Ok(())
}

async fn handle_data(
    runtime: &DaemonRuntime,
    method: &str,
    params: Value,
) -> Result<Value, HostError> {
    match method {
        DATA_INITIALIZE_METHOD => {
            let params = decode_params::<InitializeParams>(params)?;
            if params.protocol_version != CURRENT_PROTOCOL_VERSION {
                return Err(HostError::new(
                    HostErrorCode::Unsupported,
                    format!(
                        "unsupported data protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                        params.protocol_version
                    ),
                ));
            }
            encode_result(InitializeResponse {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                connection_id: runtime.next_connection_id(),
                capabilities: runtime.capabilities(),
                default_cwd: Some(runtime.config().cwd.to_string_lossy().into_owned()),
                implementation: runtime.implementation(),
            })
        }
        FS_READ_FILE_METHOD => encode_result(
            runtime
                .filesystem()
                .read_file(decode_params::<ReadFileParams>(params)?)
                .await?,
        ),
        FS_WRITE_FILE_METHOD => encode_result(
            runtime
                .filesystem()
                .write_file(decode_params::<WriteFileParams>(params)?)
                .await?,
        ),
        FS_CREATE_DIRECTORY_METHOD => encode_result(
            runtime
                .filesystem()
                .create_directory(decode_params::<CreateDirectoryParams>(params)?)
                .await?,
        ),
        FS_GET_METADATA_METHOD => encode_result(
            runtime
                .filesystem()
                .get_metadata(decode_params::<GetMetadataParams>(params)?)
                .await?,
        ),
        FS_READ_DIRECTORY_METHOD => encode_result(
            runtime
                .filesystem()
                .read_directory(decode_params::<ReadDirectoryParams>(params)?)
                .await?,
        ),
        FS_REMOVE_METHOD => encode_result(
            runtime
                .filesystem()
                .remove(decode_params::<RemoveParams>(params)?)
                .await?,
        ),
        FS_COPY_METHOD => encode_result(
            runtime
                .filesystem()
                .copy(decode_params::<CopyParams>(params)?)
                .await?,
        ),
        FS_SEARCH_TEXT_METHOD => encode_result(
            runtime
                .filesystem()
                .search_text(decode_params::<SearchTextParams>(params)?)
                .await?,
        ),
        FS_GLOB_FILES_METHOD => encode_result(
            runtime
                .filesystem()
                .glob_files(decode_params::<GlobFilesParams>(params)?)
                .await?,
        ),
        PROCESS_START_METHOD => encode_result(
            runtime
                .processes()
                .start_process(decode_params::<StartProcessParams>(params)?)
                .await?,
        ),
        PROCESS_READ_METHOD => encode_result(
            runtime
                .processes()
                .read_process(decode_params::<ReadProcessParams>(params)?)
                .await?,
        ),
        PROCESS_WRITE_METHOD => encode_result(
            runtime
                .processes()
                .write_process(decode_params::<WriteProcessParams>(params)?)
                .await?,
        ),
        PROCESS_TERMINATE_METHOD => encode_result(
            runtime
                .processes()
                .terminate_process(decode_params::<TerminateProcessParams>(params)?)
                .await?,
        ),
        PROCESS_RESIZE_METHOD => encode_result(
            runtime
                .processes()
                .resize_process(decode_params::<ResizeProcessParams>(params)?)
                .await?,
        ),
        JOB_START_METHOD => encode_result(
            runtime
                .jobs()
                .start_jobs(decode_params::<StartJobsParams>(params)?)
                .await?,
        ),
        JOB_LIST_METHOD => encode_result(
            runtime
                .jobs()
                .list_jobs(decode_params::<ListJobsParams>(params)?)
                .await?,
        ),
        JOB_READ_METHOD => encode_result(
            runtime
                .jobs()
                .read_jobs(decode_params::<ReadJobsParams>(params)?)
                .await?,
        ),
        JOB_CANCEL_METHOD => encode_result(
            runtime
                .jobs()
                .cancel_jobs(decode_params::<CancelJobsParams>(params)?)
                .await?,
        ),
        other => Err(method_not_found(other)),
    }
}

async fn recv_json<T, S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket.next().await.context("gateway closed")??;
        match message {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => bail!("gateway closed"),
        }
    }
}

async fn send_json<T, S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    value: &T,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_value(socket, serde_json::to_value(value)?).await
}

async fn send_value<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    value: Value,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .map_err(Into::into)
}

fn websocket_base(url: &str) -> anyhow::Result<String> {
    if let Some(rest) = url.strip_prefix("https://") {
        return Ok(format!("wss://{rest}"));
    }
    if let Some(rest) = url.strip_prefix("http://") {
        return Ok(format!("ws://{rest}"));
    }
    if url.starts_with("ws://") || url.starts_with("wss://") {
        return Ok(url.to_owned());
    }
    bail!("gateway URL must use http, https, ws, or wss")
}

fn tracing_line(message: &str) {
    eprintln!("lightspeed-envd {message}");
}
