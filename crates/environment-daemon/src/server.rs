use anyhow::Context as _;
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
    shared::CURRENT_PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};

pub async fn run(runtime: DaemonRuntime) -> anyhow::Result<()> {
    let listener = TcpListener::bind(runtime.config().listen)
        .await
        .context("bind environment daemon listener")?;
    tracing_line(&format!("listener ready on {}", runtime.config().listen));
    loop {
        let (stream, peer) = listener.accept().await?;
        let runtime = runtime.clone();
        tokio::spawn(async move {
            let websocket = accept_async(stream).await;
            match websocket {
                Ok(socket) => {
                    if let Err(error) = run_data_connection(&runtime, socket).await {
                        eprintln!("lightspeed-envd connection {peer} failed: {error:#}");
                    }
                }
                Err(error) => {
                    eprintln!("lightspeed-envd websocket handshake {peer} failed: {error}")
                }
            }
        });
    }
}

async fn run_data_connection<S>(
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

use crate::{
    DaemonRuntime,
    rpc::{
        decode_params, encode_result, error_response, invalid_request, method_not_found,
        parse_request, success_response,
    },
};

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

fn tracing_line(message: &str) {
    eprintln!("lightspeed-envd {message}");
}
