use std::sync::{Arc, Mutex};

use agent_api::{
    AgentApiService, AgentNotification, EventCursor, InputItem, RunStartParams, RunStatus,
    SessionEventKindView, SessionEventsReadParams, SessionItemView, SessionReadParams,
    SessionStartParams, SessionStatus,
};
use agent_core::{
    AgentHandle, ContextConfig, CoreAgentCommand, DriveCommand, LlmFinish, ModelProviderOptions,
    ModelSelection, ProviderApiKind, ProviderRequestDefaults, RunConfig, RunnerQuiescence,
    RunnerStores, SessionConfig, SessionId, SessionRunner, TurnConfig,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use agent_runtime::api::LocalAgentApi;
use agent_tools::{
    host::{
        HostToolContext, HostToolTargets, InlineHostToolRuntime,
        fs::{CreateDirectoryOptions, FileSystem, FsPath, InMemoryFileSystem},
        profiles::{HostToolPreset, resolve_host_profile_for_model},
    },
    runtime::ToolDocument,
};
use async_trait::async_trait;
use llm_clients::{ApiResponse, HeaderSnapshot, openai::responses as oai};
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesApi, OpenAiResponsesLlmAdapter};
use serde_json::{Value, json};

#[derive(Default)]
struct ScriptedOpenAiResponses {
    requests: Mutex<Vec<oai::CreateResponseRequest>>,
}

#[async_trait]
impl OpenAiResponsesApi for ScriptedOpenAiResponses {
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
        let is_followup = request_has_tool_result(&request);
        if is_followup {
            let tool_output = request
                .input
                .as_ref()
                .and_then(tool_result_output)
                .unwrap_or_default();
            assert!(
                tool_output.contains("hello from host fs"),
                "expected tool result in follow-up LLM request, got {tool_output:?}"
            );
        }
        self.requests.lock().expect("requests lock").push(request);

        let raw_json = if is_followup {
            json!({
                "id": "resp_final",
                "status": "completed",
                "output": [{
                    "id": "msg_final",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "The file says hello from host fs." }]
                }],
                "usage": { "input_tokens": 24, "output_tokens": 8, "total_tokens": 32 }
            })
        } else {
            json!({
                "id": "resp_tool",
                "status": "completed",
                "output": [{
                    "id": "fc_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"/project/note.txt\",\"offset\":null,\"limit\":null}"
                }],
                "usage": { "input_tokens": 12, "output_tokens": 6, "total_tokens": 18 }
            })
        };

        Ok(ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("scripted response shape"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        })
    }
}

struct RepeatingToolOpenAiResponses {
    requests: Mutex<Vec<oai::CreateResponseRequest>>,
    tool_rounds: usize,
}

impl RepeatingToolOpenAiResponses {
    fn new(tool_rounds: usize) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            tool_rounds,
        }
    }
}

#[async_trait]
impl OpenAiResponsesApi for RepeatingToolOpenAiResponses {
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
        let is_followup = request_has_tool_result(&request);
        if is_followup {
            let tool_output = request
                .input
                .as_ref()
                .and_then(tool_result_output)
                .unwrap_or_default();
            assert!(
                tool_output.contains("hello from host fs"),
                "expected tool result in follow-up LLM request, got {tool_output:?}"
            );
        }
        let request_index = {
            let mut requests = self.requests.lock().expect("requests lock");
            let request_index = requests.len();
            requests.push(request);
            request_index
        };

        let raw_json = if request_index < self.tool_rounds {
            let item_id = format!("fc_{}", request_index + 1);
            let call_id = format!("call_{}", request_index + 1);
            json!({
                "id": format!("resp_tool_{}", request_index + 1),
                "status": "completed",
                "output": [{
                    "id": item_id,
                    "type": "function_call",
                    "call_id": call_id,
                    "name": "read_file",
                    "arguments": "{\"path\":\"/project/note.txt\",\"offset\":null,\"limit\":null}"
                }],
                "usage": { "input_tokens": 12, "output_tokens": 6, "total_tokens": 18 }
            })
        } else {
            json!({
                "id": "resp_final",
                "status": "completed",
                "output": [{
                    "id": "msg_final",
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Completed many tool rounds." }]
                }],
                "usage": { "input_tokens": 24, "output_tokens": 8, "total_tokens": 32 }
            })
        };

        Ok(ApiResponse {
            parsed: serde_json::from_value(raw_json.clone()).expect("scripted response shape"),
            raw_json,
            status: 200,
            headers: HeaderSnapshot::default(),
        })
    }
}

fn request_has_tool_result(request: &oai::CreateResponseRequest) -> bool {
    request
        .input
        .as_ref()
        .and_then(tool_result_output)
        .is_some()
}

fn tool_result_output(input: &oai::ResponseInput) -> Option<String> {
    let oai::ResponseInput::Items(items) = input else {
        return None;
    };
    items.iter().find_map(|item| match item {
        oai::ResponseInputItem::FunctionCallOutput(output) => Some(output.output.clone()),
        oai::ResponseInputItem::Raw(value) => value
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| *kind == "function_call_output")
            .and_then(|_| value.get("output"))
            .and_then(Value::as_str)
            .map(str::to_string),
        oai::ResponseInputItem::Message(_) => None,
    })
}

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_string(),
        model: "gpt-5.1".to_string(),
        options: ModelProviderOptions::None,
    }
}

fn session_config() -> SessionConfig {
    SessionConfig {
        model: model(),
        run: RunConfig {
            max_turns: Some(4),
            max_tool_rounds: Some(4),
            model_override: None,
        },
        turn: TurnConfig {
            max_output_tokens: Some(512),
            provider_request_defaults: ProviderRequestDefaults::None,
        },
        context: ContextConfig {
            instructions_ref: None,
            max_context_tokens: None,
            target_context_tokens: None,
            reserve_output_tokens: None,
            compaction_enabled: false,
        },
        tool_profile_id: None,
    }
}

async fn store_tool_documents(blobs: &InMemoryBlobStore, documents: &[ToolDocument]) {
    for document in documents {
        let blob_ref = blobs
            .put_bytes(document.blob_write())
            .await
            .expect("store tool document");
        assert_eq!(blob_ref, document.blob_ref);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn local_runtime_drives_llm_tool_llm_loop() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let sessions = Arc::new(InMemorySessionStore::new());
    let session_id = SessionId::new("session-local");
    sessions
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("forge.default"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let fs = InMemoryFileSystem::full_access();
    fs.create_directory(
        &FsPath::new("/project").expect("project path"),
        CreateDirectoryOptions::single(),
    )
    .await
    .expect("seed directory");
    fs.write_file(
        &FsPath::new("/project/note.txt").expect("file path"),
        b"hello from host fs".to_vec(),
    )
    .await
    .expect("seed file");
    let host_ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
    let host_profile =
        resolve_host_profile_for_model(&host_ctx, &model(), HostToolPreset::DirectFs)
            .expect("host profile");
    store_tool_documents(&blobs, &host_profile.documents).await;

    let openai = Arc::new(ScriptedOpenAiResponses::default());
    let llm_adapter = Arc::new(OpenAiResponsesLlmAdapter::new(
        openai.clone(),
        blobs.clone(),
    ));
    let llm_executor = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, llm_adapter),
    ));
    let tool_executor = Arc::new(InlineHostToolRuntime::new(
        host_ctx,
        host_profile.catalog.clone(),
    ));
    let stores = RunnerStores::new(sessions, blobs.clone());
    let runner = SessionRunner::new(stores, llm_executor).with_tools(tool_executor);

    let open = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: session_config(),
            },
            max_steps: Some(32),
        })
        .await
        .expect("open session");
    assert!(open.accepted);

    let registry = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 11,
            command: CoreAgentCommand::SetToolRegistry {
                registry: host_profile.registry,
            },
            max_steps: Some(32),
        })
        .await
        .expect("set registry");
    assert!(registry.accepted);

    let target = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 12,
            command: CoreAgentCommand::SetDefaultToolTarget {
                target: HostToolTargets::local_execution_target(),
            },
            max_steps: Some(32),
        })
        .await
        .expect("set default host target");
    assert!(target.accepted);

    let selected = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 13,
            command: CoreAgentCommand::SelectToolProfile {
                profile_id: host_profile.profile_id,
            },
            max_steps: Some(32),
        })
        .await
        .expect("select profile");
    assert!(selected.accepted);

    let input_ref = blobs
        .insert_text("Read /project/note.txt and tell me what it says.")
        .await;
    let outcome = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 20,
            command: CoreAgentCommand::RequestRun {
                submission_id: None,
                input_ref,
                run_config: RunConfig {
                    max_turns: Some(4),
                    max_tool_rounds: Some(4),
                    model_override: None,
                },
            },
            max_steps: Some(128),
        })
        .await
        .expect("drive local loop");

    assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
    assert_eq!(outcome.state.runs.completed.len(), 1);
    let output_ref = outcome.state.runs.completed[0]
        .output_ref
        .clone()
        .expect("completed output");
    let output = blobs.read_text(&output_ref).await.expect("output text");
    assert_eq!(output, "The file says hello from host fs.");
    assert!(outcome.state.runs.active.is_none());
    assert_eq!(openai.requests.lock().expect("requests lock").len(), 2);

    let completed = &outcome.state.runs.completed[0];
    assert_eq!(completed.status, agent_core::RunStatus::Completed);
    assert!(outcome.emitted_entries.iter().any(|entry| {
        matches!(
            &entry.event.kind,
            agent_core::CoreAgentEventKind::Tool(agent_core::ToolEvent::CallCompleted { .. })
        )
    }));
    assert!(outcome.emitted_entries.iter().any(|entry| {
        matches!(
            &entry.event.kind,
            agent_core::CoreAgentEventKind::Turn(agent_core::TurnEvent::GenerationCompleted {
                facts,
                ..
            }) if facts.finish == LlmFinish::Stop
        )
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn local_agent_api_projects_file_tool_loop_as_session_run_items() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let sessions = Arc::new(InMemorySessionStore::new());

    let fs = InMemoryFileSystem::full_access();
    fs.create_directory(
        &FsPath::new("/project").expect("project path"),
        CreateDirectoryOptions::single(),
    )
    .await
    .expect("seed directory");
    fs.write_file(
        &FsPath::new("/project/note.txt").expect("file path"),
        b"hello from host fs".to_vec(),
    )
    .await
    .expect("seed file");
    let host_ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
    let host_profile =
        resolve_host_profile_for_model(&host_ctx, &model(), HostToolPreset::DirectFs)
            .expect("host profile");
    store_tool_documents(&blobs, &host_profile.documents).await;

    let openai = Arc::new(ScriptedOpenAiResponses::default());
    let llm_adapter = Arc::new(OpenAiResponsesLlmAdapter::new(
        openai.clone(),
        blobs.clone(),
    ));
    let llm_executor = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, llm_adapter),
    ));
    let tool_executor = Arc::new(InlineHostToolRuntime::with_targets(
        HostToolTargets::new().with_target("workspace", host_ctx),
        host_profile.catalog.clone(),
    ));
    let stores = RunnerStores::new(sessions, blobs);
    let api = LocalAgentApi::builder(stores, llm_executor, session_config())
        .with_tools(tool_executor)
        .with_tool_registry(host_profile.registry.clone())
        .with_default_tool_target(HostToolTargets::execution_target("workspace"))
        .with_tool_profile_id(host_profile.profile_id.clone())
        .build();

    let started = api
        .start_session(SessionStartParams {
            session_id: Some("session-api".to_string()),
            cwd: Some("/project".to_string()),
            ..SessionStartParams::default()
        })
        .await
        .expect("start session");
    assert_eq!(started.result.session.id, "session-api");
    assert_eq!(started.result.session.status, SessionStatus::Idle);
    assert!(matches!(
        started.notifications.as_slice(),
        [AgentNotification::SessionStarted { .. }]
    ));

    let first_events = api
        .read_session_events(SessionEventsReadParams {
            session_id: "session-api".to_string(),
            after: None,
            limit: Some(1),
        })
        .await
        .expect("read first event page");
    assert_eq!(
        first_events.result.next_cursor,
        Some(EventCursor { seq: 1 })
    );
    assert!(!first_events.result.complete);
    assert!(matches!(
        first_events.result.events.as_slice(),
        [agent_api::SessionEventView {
            kind: SessionEventKindView::SessionOpened { .. },
            ..
        }]
    ));

    let open_tail = api
        .read_session_events(SessionEventsReadParams {
            session_id: "session-api".to_string(),
            after: first_events.result.next_cursor,
            limit: Some(512),
        })
        .await
        .expect("read open tail events");
    assert!(open_tail.result.complete);
    assert!(
        open_tail
            .result
            .events
            .iter()
            .all(|event| event.cursor.seq > 1)
    );
    assert!(open_tail.result.events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKindView::ToolRegistryChanged
                | SessionEventKindView::ToolProfileSelected { .. }
        )
    }));
    let open_head = open_tail.result.head_cursor;

    let run = api
        .start_run(RunStartParams {
            session_id: "session-api".to_string(),
            input: vec![InputItem::Text {
                text: "Read /project/note.txt and tell me what it says.".to_string(),
            }],
        })
        .await
        .expect("start run");

    assert_eq!(run.result.run.id, "run_1");
    assert_eq!(run.result.run.status, RunStatus::Completed);
    assert!(run.result.run.items.iter().any(|item| {
        matches!(
            item,
            SessionItemView::AssistantMessage { text, .. }
                if text == "The file says hello from host fs."
        )
    }));
    assert!(run.result.run.items.iter().any(|item| {
        matches!(
            item,
            SessionItemView::ToolCall {
                status: agent_api::ToolItemStatus::Requested,
                ..
            }
        )
    }));
    assert_eq!(run.result.run.tool_batches.len(), 1);
    let batch = &run.result.run.tool_batches[0];
    assert_eq!(batch.status, agent_api::ToolItemStatus::Succeeded);
    assert_eq!(batch.calls.len(), 1);
    assert_eq!(batch.calls[0].tool_name, "read_file");
    assert_eq!(
        batch.calls[0].display.as_ref().map(|display| {
            (
                display.group,
                display.verb.as_str(),
                display.target.as_deref(),
            )
        }),
        Some((
            agent_api::ToolCallDisplayGroup::Explore,
            "Read",
            Some("/project/note.txt")
        ))
    );
    assert_eq!(batch.calls[0].status, agent_api::ToolItemStatus::Succeeded);
    assert!(
        batch.calls[0]
            .arguments
            .as_deref()
            .is_some_and(|args| args.contains("/project/note.txt"))
    );
    assert!(
        batch.calls[0]
            .output
            .as_deref()
            .is_some_and(|output| output.contains("hello from host fs"))
    );
    assert!(run.notifications.iter().any(|notification| {
        matches!(
            notification,
            AgentNotification::ItemCompleted {
                item: SessionItemView::ToolResult { output: Some(output), .. },
                ..
            } if output.contains("hello from host fs")
        )
    }));
    assert!(run.notifications.iter().any(|notification| {
        matches!(
            notification,
            AgentNotification::SessionStatusChanged {
                status: SessionStatus::Idle,
                ..
            }
        )
    }));

    let run_events = api
        .read_session_events(SessionEventsReadParams {
            session_id: "session-api".to_string(),
            after: open_head,
            limit: Some(512),
        })
        .await
        .expect("read run events");
    assert!(run_events.result.complete);
    assert!(run_events.result.events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKindView::RunStarted { run_id, .. } if run_id == "run_1"
        )
    }));
    assert!(run_events.result.events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKindView::ItemsRecorded { items }
                if items.iter().any(|item| matches!(
                    item,
                    SessionItemView::AssistantMessage { text, .. }
                        if text == "The file says hello from host fs."
                ))
        )
    }));
    assert!(run_events.result.events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKindView::ToolBatchStarted { calls, .. }
                if calls.iter().any(|call| {
                    call.tool_name == "read_file"
                        && call
                            .display
                            .as_ref()
                            .is_some_and(|display| display.verb == "Read")
                        && call
                            .arguments
                            .as_deref()
                            .is_some_and(|args| args.contains("/project/note.txt"))
                })
        )
    }));
    assert!(run_events.result.events.iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKindView::RunCompleted { run_id, .. } if run_id == "run_1"
        )
    }));

    let read = api
        .read_session(SessionReadParams {
            session_id: "session-api".to_string(),
        })
        .await
        .expect("read session");
    assert_eq!(read.result.session.runs.len(), 1);
    assert_eq!(read.result.session.runs[0].status, RunStatus::Completed);
    assert_eq!(read.result.session.runs[0].tool_batches.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn local_agent_api_continues_after_runner_step_slice_limit() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let sessions = Arc::new(InMemorySessionStore::new());

    let fs = InMemoryFileSystem::full_access();
    fs.create_directory(
        &FsPath::new("/project").expect("project path"),
        CreateDirectoryOptions::single(),
    )
    .await
    .expect("seed directory");
    fs.write_file(
        &FsPath::new("/project/note.txt").expect("file path"),
        b"hello from host fs".to_vec(),
    )
    .await
    .expect("seed file");
    let host_ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone());
    let host_profile =
        resolve_host_profile_for_model(&host_ctx, &model(), HostToolPreset::DirectFs)
            .expect("host profile");
    store_tool_documents(&blobs, &host_profile.documents).await;

    let tool_rounds = 30;
    let openai = Arc::new(RepeatingToolOpenAiResponses::new(tool_rounds));
    let llm_adapter = Arc::new(OpenAiResponsesLlmAdapter::new(
        openai.clone(),
        blobs.clone(),
    ));
    let llm_executor = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, llm_adapter),
    ));
    let tool_executor = Arc::new(InlineHostToolRuntime::new(
        host_ctx,
        host_profile.catalog.clone(),
    ));
    let stores = RunnerStores::new(sessions, blobs);
    let api = LocalAgentApi::builder(stores, llm_executor, session_config())
        .with_tools(tool_executor)
        .with_tool_registry(host_profile.registry.clone())
        .with_default_tool_target(HostToolTargets::local_execution_target())
        .with_tool_profile_id(host_profile.profile_id.clone())
        .build();

    api.start_session(SessionStartParams {
        session_id: Some("session-many-rounds".to_string()),
        cwd: Some("/project".to_string()),
        ..SessionStartParams::default()
    })
    .await
    .expect("start session");

    let run = api
        .start_run(RunStartParams {
            session_id: "session-many-rounds".to_string(),
            input: vec![InputItem::Text {
                text: "Keep reading the note until you are done.".to_string(),
            }],
        })
        .await
        .expect("start run");

    assert_eq!(run.result.run.status, RunStatus::Completed);
    assert_eq!(run.result.run.tool_batches.len(), tool_rounds);
    assert!(run.result.run.items.iter().any(|item| {
        matches!(
            item,
            SessionItemView::AssistantMessage { text, .. }
                if text == "Completed many tool rounds."
        )
    }));
    assert_eq!(
        openai.requests.lock().expect("requests lock").len(),
        tool_rounds + 1
    );
}
