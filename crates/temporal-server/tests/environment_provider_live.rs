mod support;

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use api::{
    AgentApiService, EnvironmentProviderCapabilitiesView, EnvironmentProviderHeartbeatParams,
    EnvironmentProviderImplementationView, EnvironmentProviderKindView,
    EnvironmentProviderRegisterParams, HostControllerConnectionView, HostTargetAttachRequestView,
    HostTargetCreateRequestView, HostTransportView, InputItem, RunStartParams, RunStatus,
    SandboxTargetSpecView, SessionConfigInput, SessionEnvironmentAttachParams,
    SessionEnvironmentCloseParams, SessionEnvironmentCreateParams, SessionStartParams,
};
use async_trait::async_trait;
use engine::{
    ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextMessageRole, CoreAgentIoError,
    CoreAgentLlm, CoreAgentTools, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, ModelSelection, ObservedToolCall, ProviderApiKind,
    ToolCallId, ToolName, storage::BlobStore,
};
use futures::{SinkExt, StreamExt};
use host_protocol::{
    control::{
        handshake::{ControllerCapabilities, ControllerInitializeResponse},
        methods::{
            ATTACH_TARGET_METHOD, CLOSE_TARGET_METHOD, CREATE_TARGET_METHOD,
            INITIALIZE_METHOD as CONTROL_INITIALIZE_METHOD, LIST_TARGETS_METHOD,
        },
        targets::{
            AttachTargetResponse, CloseTargetResponse, CreateTargetResponse, HostTargetStatus,
            HostTargetSummary, ListTargetsResponse,
        },
    },
    data::{
        handshake::{InitializeResponse, InitializedParams},
        methods::{
            INITIALIZE_METHOD as DATA_INITIALIZE_METHOD, INITIALIZED_METHOD, PROCESS_READ_METHOD,
            PROCESS_START_METHOD,
        },
        process::{
            ProcessOutputChunk, ProcessOutputStream, ReadProcessResponse, StartProcessParams,
            StartProcessResponse,
        },
    },
    shared::{
        ByteChunk, CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionId,
        HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport, ImplementationInfo,
    },
};
use serde_json::{Value, json};
use support::live::{LIVE_TEST_LOCK, final_assistant_text, require_storage_live_env};
use temporal_server::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{ActivityState, SessionTools, WorkerActivities},
};
use temporal_workflow::AgentSessionWorkflow;
use temporalio_client::{Client, WorkflowTerminateOptions};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const ATTACH_TARGET_ID: &str = "attach-target";
const CREATED_TARGET_ID: &str = "created-target";
const PROCESS_STDOUT: &str = "fake provider stdout\n";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn temporal_live_fake_provider_create_attach_and_process_tool() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let provider = FakeHostProvider::start().await?;
    let store = pg_store_from_env().await?;
    let blobs: Arc<dyn BlobStore> = store.clone();
    let llm = Arc::new(ExecCommandLlm::new(blobs.clone())) as Arc<dyn CoreAgentLlm>;
    let tools = Arc::new(SessionTools::from_pg_store(store.clone())) as Arc<dyn CoreAgentTools>;
    let activities = WorkerActivities::new(ActivityState::from_pg_store(store, llm, tools));

    support::live::run_with_live_worker(activities, |client, task_queue, session_id| async move {
        run_fake_provider_client(client, task_queue, session_id, provider).await
    })
    .await
}

async fn run_fake_provider_client(
    client: Client,
    task_queue: String,
    session_id: engine::SessionId,
    provider: FakeHostProvider,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = fake_model();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(32)
        .build();
    let provider_id = format!("fake-provider-{}", uuid::Uuid::new_v4().simple());

    let registered = api
        .register_environment_provider(EnvironmentProviderRegisterParams {
            provider_id: provider_id.clone(),
            provider_kind: EnvironmentProviderKindView::Bridge,
            controller_connection: HostControllerConnectionView {
                endpoint: provider.endpoint().to_owned(),
                transport: HostTransportView::WebSocket,
            },
            capabilities: EnvironmentProviderCapabilitiesView::default(),
            implementation: EnvironmentProviderImplementationView {
                name: "client-supplied-placeholder".to_owned(),
                version: None,
            },
            lease_ttl_ms: 60_000,
            display_name: Some("fake host provider".to_owned()),
            metadata: BTreeMap::new(),
        })
        .await?;
    assert!(registered.result.provider.capabilities.create_target);
    assert!(registered.result.provider.capabilities.attach_target);
    assert_eq!(
        registered.result.provider.implementation.name,
        "fake-host-provider"
    );
    assert_eq!(provider.controller_initialize_count(), 1);

    let heartbeat = api
        .heartbeat_environment_provider(EnvironmentProviderHeartbeatParams {
            provider_id: provider_id.clone(),
            lease_ttl_ms: None,
            observed_targets: Vec::new(),
        })
        .await?;
    assert_eq!(heartbeat.result.targets.len(), 1);
    assert_eq!(heartbeat.result.targets[0].target_id, ATTACH_TARGET_ID);
    assert_eq!(provider.list_targets_count(), 1);

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(api_projection::model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
    })
    .await?;

    let attached = api
        .attach_session_environment(SessionEnvironmentAttachParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("bridge-env".to_owned()),
            provider_id: provider_id.clone(),
            request: HostTargetAttachRequestView::Target {
                target_id: ATTACH_TARGET_ID.to_owned(),
            },
            activate: true,
        })
        .await?;
    assert_eq!(attached.result.active_env_id.as_deref(), Some("bridge-env"));
    assert_eq!(provider.attach_count(), 1);

    let first = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "run a command in the attached provider target".to_owned(),
            }],
            config: None,
        })
        .await?;
    let first_run =
        support::live::wait_for_terminal_run(&api, &session_id, &first.result.run.id).await?;
    assert_eq!(
        first_run.status,
        RunStatus::Completed,
        "first run did not complete: {first_run:#?}"
    );
    let Some(first_text) = final_assistant_text(&first_run) else {
        anyhow::bail!("first run missing final assistant message: {first_run:#?}");
    };
    assert!(first_text.contains(PROCESS_STDOUT));

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "bridge-env".to_owned(),
        force: false,
        close_target: Some(false),
    })
    .await?;
    assert_eq!(
        provider.close_count(),
        0,
        "bridge detach should not close target when close_target=false"
    );

    let created = api
        .create_session_environment(SessionEnvironmentCreateParams {
            session_id: session_id.as_str().to_owned(),
            env_id: Some("sandbox-env".to_owned()),
            provider_id: provider_id.clone(),
            request: HostTargetCreateRequestView::Sandbox {
                spec: SandboxTargetSpecView {
                    image: Some("fake-image".to_owned()),
                    cwd: Some("/workspace".to_owned()),
                    ..SandboxTargetSpecView::default()
                },
            },
            activate: true,
        })
        .await?;
    assert_eq!(created.result.active_env_id.as_deref(), Some("sandbox-env"));
    assert_eq!(provider.create_count(), 1);

    let second = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Text {
                text: "run a command in the created provider target".to_owned(),
            }],
            config: None,
        })
        .await?;
    let second_run =
        support::live::wait_for_terminal_run(&api, &session_id, &second.result.run.id).await?;
    assert_eq!(
        second_run.status,
        RunStatus::Completed,
        "second run did not complete: {second_run:#?}"
    );
    let Some(second_text) = final_assistant_text(&second_run) else {
        anyhow::bail!("second run missing final assistant message: {second_run:#?}");
    };
    assert!(second_text.contains(PROCESS_STDOUT));

    api.close_session_environment(SessionEnvironmentCloseParams {
        session_id: session_id.as_str().to_owned(),
        env_id: "sandbox-env".to_owned(),
        force: false,
        close_target: None,
    })
    .await?;
    assert_eq!(provider.close_count(), 1);
    assert_eq!(provider.process_start_count(), 2);
    assert_eq!(
        provider.process_cwds(),
        vec![Some("/workspace".to_owned()), Some("/workspace".to_owned())]
    );

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("fake provider live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

fn fake_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "fake".to_owned(),
        model: "fake-env-tool-model".to_owned(),
    }
}

struct ExecCommandLlm {
    blobs: Arc<dyn BlobStore>,
}

impl ExecCommandLlm {
    fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self { blobs }
    }

    async fn tool_call_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if !request
            .request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "exec_command")
        {
            return Err(io_error("planned request did not expose exec_command"));
        }
        let arguments = json!({
            "argv": ["fake-provider-command"],
            "yield_time_ms": 1,
            "max_output_bytes": 4096
        });
        let arguments_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&arguments).map_err(io_error)?)
            .await
            .map_err(io_error)?;
        let call_id = ToolCallId::new(format!("env_call_{}_{}", request.run_id, request.turn_id));
        let tool_name = ToolName::new("exec_command");
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::ToolCall {
                    call_id: call_id.clone(),
                    name: tool_name.clone(),
                },
                content_ref: arguments_ref.clone(),
                media_type: Some("application/json".to_owned()),
                preview: Some(format!("exec_command({arguments})")),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: Some(call_id.as_str().to_owned()),
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-tool-{}", request.turn_id)),
                finish: LlmFinish::ToolCalls,
                usage: None,
                tool_calls: vec![ObservedToolCall {
                    call_id,
                    tool_name,
                    provider_kind: Some("fake".to_owned()),
                    arguments_ref,
                    native_call_ref: None,
                }],
                context_token_estimate: None,
            },
        })
    }

    async fn final_result(
        &self,
        request: &LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let tool_output = if let Some(entry) = current_run_tool_result(request) {
            self.blobs
                .read_text(&entry.content_ref)
                .await
                .map_err(io_error)?
        } else {
            "no tool result".to_owned()
        };
        let text = format!("Fake provider run completed with output:\n{tool_output}");
        let output_ref = self
            .blobs
            .put_bytes(text.into_bytes())
            .await
            .map_err(io_error)?;
        Ok(LlmGenerationResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                content_ref: output_ref,
                media_type: Some("text/plain".to_owned()),
                preview: Some("fake provider final answer".to_owned()),
                provider_kind: Some("fake".to_owned()),
                provider_item_id: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                provider_response_id: Some(format!("fake-final-{}", request.turn_id)),
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                context_token_estimate: None,
            },
        })
    }
}

#[async_trait]
impl CoreAgentLlm for ExecCommandLlm {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        if current_run_tool_result(&request).is_some() {
            self.final_result(&request).await
        } else {
            self.tool_call_result(&request).await
        }
    }
}

fn current_run_tool_result(request: &LlmGenerationRequest) -> Option<&engine::ContextEntry> {
    request.request.context.entries.iter().rev().find(|entry| {
        matches!(
            (&entry.source, &entry.kind),
            (
                ContextEntrySource::Tool { run_id, .. },
                ContextEntryKind::ToolResult { .. }
            ) if *run_id == request.run_id
        )
    })
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

struct FakeHostProvider {
    endpoint: String,
    state: Arc<FakeHostProviderState>,
    server: JoinHandle<()>,
}

impl FakeHostProvider {
    async fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let state = Arc::new(FakeHostProviderState::default());
        let server_state = state.clone();
        let server_endpoint = endpoint.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(handle_connection(
                    stream,
                    server_state.clone(),
                    server_endpoint.clone(),
                ));
            }
        });
        Ok(Self {
            endpoint,
            state,
            server,
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn controller_initialize_count(&self) -> usize {
        self.state
            .controller_initialize_count
            .load(Ordering::SeqCst)
    }

    fn list_targets_count(&self) -> usize {
        self.state.list_targets_count.load(Ordering::SeqCst)
    }

    fn attach_count(&self) -> usize {
        self.state.attach_count.load(Ordering::SeqCst)
    }

    fn create_count(&self) -> usize {
        self.state.create_count.load(Ordering::SeqCst)
    }

    fn close_count(&self) -> usize {
        self.state.close_count.load(Ordering::SeqCst)
    }

    fn process_start_count(&self) -> usize {
        self.state
            .process_starts
            .lock()
            .expect("process starts")
            .len()
    }

    fn process_cwds(&self) -> Vec<Option<String>> {
        self.state
            .process_starts
            .lock()
            .expect("process starts")
            .iter()
            .map(|params| params.cwd.as_ref().map(|cwd| cwd.as_str().to_owned()))
            .collect()
    }
}

impl Drop for FakeHostProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Default)]
struct FakeHostProviderState {
    controller_initialize_count: AtomicUsize,
    list_targets_count: AtomicUsize,
    attach_count: AtomicUsize,
    create_count: AtomicUsize,
    close_count: AtomicUsize,
    process_starts: Mutex<Vec<StartProcessParams>>,
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: Arc<FakeHostProviderState>,
    endpoint: String,
) {
    let Ok(mut socket) = accept_async(stream).await else {
        return;
    };
    while let Some(message) = socket.next().await {
        let Ok(message) = message else {
            return;
        };
        let Ok(value) = websocket_json(message) else {
            continue;
        };
        let Some(id) = value.get("id").cloned() else {
            if value.get("method").and_then(Value::as_str) == Some(INITIALIZED_METHOD) {
                let _ = serde_json::from_value::<InitializedParams>(
                    value.get("params").cloned().unwrap_or(Value::Null),
                );
            }
            continue;
        };
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let response = match handle_request(method, params, state.as_ref(), &endpoint).await {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": "internal",
                    "message": message
                }
            }),
        };
        if socket
            .send(Message::Text(response.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn websocket_json(message: Message) -> anyhow::Result<Value> {
    match message {
        Message::Text(text) => Ok(serde_json::from_str(&text)?),
        Message::Binary(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Message::Close(_) => anyhow::bail!("websocket closed"),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
            anyhow::bail!("control frame")
        }
    }
}

async fn handle_request(
    method: &str,
    params: Value,
    state: &FakeHostProviderState,
    endpoint: &str,
) -> Result<Value, String> {
    match method {
        CONTROL_INITIALIZE_METHOD => {
            state
                .controller_initialize_count
                .fetch_add(1, Ordering::SeqCst);
            result_value(ControllerInitializeResponse {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                capabilities: ControllerCapabilities {
                    list_targets: true,
                    create_target: true,
                    attach_target: true,
                    get_target: true,
                    close_target: true,
                },
                implementation: ImplementationInfo {
                    name: "fake-host-provider".to_owned(),
                    version: Some("test".to_owned()),
                },
            })
        }
        LIST_TARGETS_METHOD => {
            state.list_targets_count.fetch_add(1, Ordering::SeqCst);
            result_value(ListTargetsResponse {
                targets: vec![target_summary(ATTACH_TARGET_ID)],
            })
        }
        ATTACH_TARGET_METHOD => {
            state.attach_count.fetch_add(1, Ordering::SeqCst);
            result_value(AttachTargetResponse {
                target: target_summary(ATTACH_TARGET_ID),
                connection: connection_spec(endpoint, ATTACH_TARGET_ID),
            })
        }
        CREATE_TARGET_METHOD => {
            state.create_count.fetch_add(1, Ordering::SeqCst);
            result_value(CreateTargetResponse {
                target: target_summary(CREATED_TARGET_ID),
                connection: connection_spec(endpoint, CREATED_TARGET_ID),
            })
        }
        CLOSE_TARGET_METHOD => {
            state.close_count.fetch_add(1, Ordering::SeqCst);
            result_value(CloseTargetResponse {
                target_id: HostTargetId::new(
                    params
                        .get("targetId")
                        .and_then(Value::as_str)
                        .unwrap_or(CREATED_TARGET_ID),
                ),
                status: HostTargetStatus::Closed,
            })
        }
        DATA_INITIALIZE_METHOD => result_value(InitializeResponse {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            connection_id: HostConnectionId::new("fake-data-connection"),
            capabilities: host_capabilities(),
            default_cwd: Some("/workspace".to_owned()),
            implementation: ImplementationInfo {
                name: "fake-host-data".to_owned(),
                version: Some("test".to_owned()),
            },
        }),
        PROCESS_START_METHOD => {
            let params: StartProcessParams =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            let process_id = params.process_id.clone();
            state
                .process_starts
                .lock()
                .map_err(|error| error.to_string())?
                .push(params);
            result_value(StartProcessResponse { process_id })
        }
        PROCESS_READ_METHOD => result_value(ReadProcessResponse {
            chunks: vec![ProcessOutputChunk {
                seq: 1,
                stream: ProcessOutputStream::Stdout,
                chunk: ByteChunk::new(PROCESS_STDOUT.as_bytes()),
            }],
            next_seq: 2,
            exited: true,
            exit_code: Some(0),
            closed: true,
            failure: None,
        }),
        other => Err(format!("unsupported fake host method: {other}")),
    }
}

fn result_value(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn target_summary(target_id: &str) -> HostTargetSummary {
    HostTargetSummary {
        target_id: HostTargetId::new(target_id),
        display_name: Some(target_id.to_owned()),
        status: HostTargetStatus::Ready,
        scope: HostScope::Default,
        capabilities: host_capabilities(),
        default_cwd: Some(HostPath::new("/workspace").expect("host cwd")),
        metadata: BTreeMap::new(),
    }
}

fn connection_spec(endpoint: &str, target_id: &str) -> HostConnectionSpec {
    HostConnectionSpec {
        target_id: HostTargetId::new(target_id),
        endpoint: endpoint.to_owned(),
        transport: HostTransport::WebSocket,
        scope: HostScope::Default,
        default_cwd: Some(HostPath::new("/workspace").expect("host cwd")),
        capabilities: host_capabilities(),
    }
}

fn host_capabilities() -> HostCapabilities {
    HostCapabilities {
        filesystem_read: true,
        filesystem_write: true,
        process_start: true,
        process_stdin: true,
        process_terminate: true,
        process_output_polling: true,
        process_output_notifications: false,
        process_pty: false,
    }
}
