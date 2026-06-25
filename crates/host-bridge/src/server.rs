use std::{
    net::SocketAddr,
    sync::{Arc, Mutex as StdMutex},
};

use futures_util::{SinkExt, StreamExt};
use host_protocol::{
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        methods::{
            ATTACH_TARGET_METHOD, CLOSE_TARGET_METHOD, CREATE_TARGET_METHOD, GET_TARGET_METHOD,
            INITIALIZE_METHOD as CONTROL_INITIALIZE_METHOD, LIST_TARGETS_METHOD,
        },
        targets::{
            AttachTargetParams, AttachTargetResponse, CloseTargetParams, CloseTargetResponse,
            CreateTargetParams, GetTargetParams, GetTargetResponse, HostTargetAttachRequest,
            HostTargetStatus, ListTargetsParams, ListTargetsResponse,
        },
    },
    data::{
        fs::{
            CopyParams, CreateDirectoryParams, GetMetadataParams, ReadDirectoryParams,
            ReadFileParams, RemoveParams, WriteFileParams,
        },
        handshake::{InitializeParams, InitializeResponse, InitializedParams},
        jobs::{CancelJobsParams, ListJobsParams, ReadJobsParams, StartJobsParams},
        methods::{
            FS_COPY_METHOD, FS_CREATE_DIRECTORY_METHOD, FS_GET_METADATA_METHOD,
            FS_READ_DIRECTORY_METHOD, FS_READ_FILE_METHOD, FS_REMOVE_METHOD, FS_WRITE_FILE_METHOD,
            INITIALIZE_METHOD as DATA_INITIALIZE_METHOD, INITIALIZED_METHOD, JOB_CANCEL_METHOD,
            JOB_LIST_METHOD, JOB_READ_METHOD, JOB_START_METHOD, PROCESS_READ_METHOD,
            PROCESS_START_METHOD, PROCESS_TERMINATE_METHOD, PROCESS_WRITE_METHOD,
        },
        process::{
            ReadProcessParams, StartProcessParams, TerminateProcessParams, WriteProcessParams,
        },
    },
    error::{HostError, HostErrorCode},
    shared::{CURRENT_PROTOCOL_VERSION, HostScope},
};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

use crate::{
    BridgeRuntime,
    rpc::{
        decode_params, encode_result, error_response, invalid_request, method_not_found, not_found,
        parse_request, success_response, unsupported,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Plane {
    Control,
    Data,
}

pub async fn run_server(listener: TcpListener, runtime: BridgeRuntime) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let runtime = runtime.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, peer, runtime).await {
                eprintln!("host-bridge connection failed: {error}");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    _peer: SocketAddr,
    runtime: BridgeRuntime,
) -> anyhow::Result<()> {
    let path = Arc::new(StdMutex::new(String::new()));
    let path_for_callback = path.clone();
    let mut socket = accept_hdr_async(stream, move |request: &Request, response: Response| {
        if let Ok(mut path) = path_for_callback.lock() {
            *path = request.uri().path().to_owned();
        }
        Ok(response)
    })
    .await?;
    let path = path
        .lock()
        .map(|path| path.clone())
        .unwrap_or_else(|_| String::new());
    let plane = match path.as_str() {
        "/control" => Plane::Control,
        "/data" => Plane::Data,
        other => {
            let response = error_response(
                None,
                HostError::new(
                    HostErrorCode::InvalidRequest,
                    format!("unsupported host-bridge WebSocket path: {other}"),
                ),
            );
            let _ = socket
                .send(Message::Text(response.to_string().into()))
                .await;
            return Ok(());
        }
    };

    while let Some(message) = socket.next().await {
        let message = message?;
        let value = match websocket_json(message) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let request = match parse_request(value) {
            Ok(request) => request,
            Err(error) => {
                socket
                    .send(Message::Text(
                        error_response(None, error).to_string().into(),
                    ))
                    .await?;
                continue;
            }
        };
        let Some(id) = request.id else {
            handle_notification(plane, request.method.as_deref(), request.params);
            continue;
        };
        let Some(method) = request.method else {
            socket
                .send(Message::Text(
                    error_response(Some(id), invalid_request("request missing method"))
                        .to_string()
                        .into(),
                ))
                .await?;
            continue;
        };
        let result = match plane {
            Plane::Control => handle_control(&runtime, &method, request.params).await,
            Plane::Data => handle_data(&runtime, &method, request.params).await,
        };
        let response = match result {
            Ok(result) => success_response(id, result),
            Err(error) => error_response(Some(id), error),
        };
        socket
            .send(Message::Text(response.to_string().into()))
            .await?;
    }

    Ok(())
}

fn handle_notification(plane: Plane, method: Option<&str>, params: Value) {
    if plane == Plane::Data && method == Some(INITIALIZED_METHOD) {
        let _ = decode_params::<InitializedParams>(params);
    }
}

async fn handle_control(
    runtime: &BridgeRuntime,
    method: &str,
    params: Value,
) -> Result<Value, HostError> {
    match method {
        CONTROL_INITIALIZE_METHOD => {
            let params = decode_params::<ControllerInitializeParams>(params)?;
            if params.protocol_version != CURRENT_PROTOCOL_VERSION {
                return Err(HostError::new(
                    HostErrorCode::Unsupported,
                    format!(
                        "unsupported controller protocol version {}; expected {CURRENT_PROTOCOL_VERSION}",
                        params.protocol_version
                    ),
                ));
            }
            encode_result(ControllerInitializeResponse {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                capabilities: runtime.controller_capabilities(),
                implementation: runtime.implementation(),
            })
        }
        LIST_TARGETS_METHOD => {
            let params = decode_params::<ListTargetsParams>(params)?;
            let target = runtime
                .target_summary()
                .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?;
            let targets = if params.status.is_none_or(|status| status == target.status) {
                vec![target]
            } else {
                Vec::new()
            };
            encode_result(ListTargetsResponse { targets })
        }
        ATTACH_TARGET_METHOD => {
            let params = decode_params::<AttachTargetParams>(params)?;
            let target_id = match params.request {
                HostTargetAttachRequest::Target { target_id } => target_id,
                HostTargetAttachRequest::Provider { .. } => {
                    return Err(unsupported(
                        "host-bridge only supports attachTarget by target id",
                    ));
                }
            };
            if target_id != runtime.target_id() {
                return Err(not_found(format!("unknown target id: {target_id}")));
            }
            encode_result(AttachTargetResponse {
                target: runtime
                    .target_summary()
                    .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?,
                connection: runtime
                    .connection_spec()
                    .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?,
            })
        }
        GET_TARGET_METHOD => {
            let params = decode_params::<GetTargetParams>(params)?;
            if params.target_id != runtime.target_id() {
                return Err(not_found(format!(
                    "unknown target id: {}",
                    params.target_id
                )));
            }
            encode_result(GetTargetResponse {
                target: runtime
                    .target_summary()
                    .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?,
            })
        }
        CLOSE_TARGET_METHOD => {
            let params = decode_params::<CloseTargetParams>(params)?;
            if params.target_id != runtime.target_id() {
                return Err(not_found(format!(
                    "unknown target id: {}",
                    params.target_id
                )));
            }
            encode_result(CloseTargetResponse {
                target_id: params.target_id,
                status: HostTargetStatus::Closed,
            })
        }
        CREATE_TARGET_METHOD => {
            let _ = decode_params::<CreateTargetParams>(params)?;
            Err(unsupported(
                "host-bridge is attach-only and does not create targets",
            ))
        }
        other => Err(method_not_found(other)),
    }
}

async fn handle_data(
    runtime: &BridgeRuntime,
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
            let scope = match params.scope {
                HostScope::Default => HostScope::Default,
                HostScope::Session { session_id } => HostScope::Session { session_id },
            };
            let default_cwd = runtime
                .connection_spec()
                .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?
                .default_cwd
                .map(|path| path.to_string());
            encode_result(InitializeResponse {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                connection_id: runtime.next_connection_id(),
                capabilities: runtime.capabilities(),
                default_cwd,
                implementation: runtime.implementation(),
            })
            .map(|value| {
                let _ = scope;
                value
            })
        }
        FS_READ_FILE_METHOD => {
            let params = decode_params::<ReadFileParams>(params)?;
            encode_result(runtime.filesystem().read_file(params).await?)
        }
        FS_WRITE_FILE_METHOD => {
            let params = decode_params::<WriteFileParams>(params)?;
            encode_result(runtime.filesystem().write_file(params).await?)
        }
        FS_CREATE_DIRECTORY_METHOD => {
            let params = decode_params::<CreateDirectoryParams>(params)?;
            encode_result(runtime.filesystem().create_directory(params).await?)
        }
        FS_GET_METADATA_METHOD => {
            let params = decode_params::<GetMetadataParams>(params)?;
            encode_result(runtime.filesystem().get_metadata(params).await?)
        }
        FS_READ_DIRECTORY_METHOD => {
            let params = decode_params::<ReadDirectoryParams>(params)?;
            encode_result(runtime.filesystem().read_directory(params).await?)
        }
        FS_REMOVE_METHOD => {
            let params = decode_params::<RemoveParams>(params)?;
            encode_result(runtime.filesystem().remove(params).await?)
        }
        FS_COPY_METHOD => {
            let params = decode_params::<CopyParams>(params)?;
            encode_result(runtime.filesystem().copy(params).await?)
        }
        PROCESS_START_METHOD => {
            let params = decode_params::<StartProcessParams>(params)?;
            encode_result(runtime.processes().start_process(params).await?)
        }
        PROCESS_READ_METHOD => {
            let params = decode_params::<ReadProcessParams>(params)?;
            encode_result(runtime.processes().read_process(params).await?)
        }
        PROCESS_WRITE_METHOD => {
            let params = decode_params::<WriteProcessParams>(params)?;
            encode_result(runtime.processes().write_process(params).await?)
        }
        PROCESS_TERMINATE_METHOD => {
            let params = decode_params::<TerminateProcessParams>(params)?;
            encode_result(runtime.processes().terminate_process(params).await?)
        }
        JOB_START_METHOD => {
            let params = decode_params::<StartJobsParams>(params)?;
            encode_result(runtime.jobs().start_jobs(params).await?)
        }
        JOB_LIST_METHOD => {
            let params = decode_params::<ListJobsParams>(params)?;
            encode_result(runtime.jobs().list_jobs(params).await?)
        }
        JOB_READ_METHOD => {
            let params = decode_params::<ReadJobsParams>(params)?;
            encode_result(runtime.jobs().read_jobs(params).await?)
        }
        JOB_CANCEL_METHOD => {
            let params = decode_params::<CancelJobsParams>(params)?;
            encode_result(runtime.jobs().cancel_jobs(params).await?)
        }
        other => Err(method_not_found(other)),
    }
}

fn websocket_json(message: Message) -> anyhow::Result<Value> {
    match message {
        Message::Text(text) => Ok(serde_json::from_str(&text)?),
        Message::Binary(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Message::Close(_) => anyhow::bail!("websocket closed"),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => anyhow::bail!("control frame"),
    }
}
