use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use engine::{
    AgentHandle, ContextConfig, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    CoreAgentCommand, CoreAgentEventKind, ModelSelection, ProviderApiKind, RemoteMcpApprovalPolicy,
    RemoteMcpToolSpec, RunConfig, RunStatus, SessionConfig, SessionId, ToolKind, ToolName,
    ToolParallelism, ToolSpec, ToolTargetRequirement,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_clients::openai::responses::{Client, Config};
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter};
use serde_json::Value;
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};

mod support;

use support::retrying_openai_responses_client;

const MCP_ECHO_SERVER_URL: &str = "https://mcpplaygroundonline.com/mcp-echo-server";
const MCP_ECHO_MARKER: &str = "LIGHTSPEED-MCP-ECHO-LIVE-7392";

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("OPENAI_API_KEY").expect(
        "OPENAI_API_KEY must be set in env or root .env to run llm-runtime OpenAI MCP live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "OPENAI_API_KEY is set but empty"
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("OPENAI_BASE_URL") {
        config.base_url = base_url;
    }
    if let Ok(org_id) = env_or_dotenv_var("OPENAI_ORG_ID") {
        config.organization = Some(org_id);
    }
    if let Ok(project) = env_or_dotenv_var("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }

    Client::new(config).expect("OpenAI Responses client")
}

fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(root_dotenv_path()).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() == name {
            return Some(unquote_dotenv_value(value.trim()));
        }
    }
    None
}

fn root_dotenv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .join(".env")
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and public MCP server access (costs real money)"]
async fn openai_responses_live_core_session_uses_no_auth_remote_mcp_echo() {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let session_id = SessionId::new("session-live-mcp-echo");
    sessions
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.live-mcp"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let model = ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_string(),
        model: live_model(),
    };
    let llm = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new().with_generation_adapter(
            ProviderApiKind::OpenAiResponses,
            Arc::new(OpenAiResponsesLlmAdapter::new(
                retrying_openai_responses_client(live_client()),
                blobs.clone(),
            )),
        ),
    ));
    let stores = RunnerStores::new(sessions.clone(), blobs.clone());
    let runner = SessionRunner::new(stores, llm);

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: session_config(model),
            },
            max_steps: None,
        })
        .await
        .expect("open session");
    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 11,
            command: CoreAgentCommand::ReplaceTools {
                expected_revision: Some(0),
                tools: remote_mcp_echo_tools(),
            },
            max_steps: None,
        })
        .await
        .expect("replace tools");

    let input_ref = blobs
        .put_bytes(
            format!(
                "Use the remote MCP server labeled echo. It exposes an MCP tool named echo. \
                 Call that MCP tool with JSON arguments exactly {{\"data\":\"{MCP_ECHO_MARKER}\"}}. \
                 After the tool returns, reply exactly ECHO={MCP_ECHO_MARKER}."
            )
            .into_bytes(),
        )
        .await
        .expect("write prompt");
    let outcome = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 20,
            command: CoreAgentCommand::RequestRun {
                submission_id: None,
                input: vec![ContextEntryInput {
                    kind: ContextEntryKind::Message {
                        role: ContextMessageRole::User,
                    },
                    content_ref: input_ref,
                    media_type: None,
                    preview: None,
                    provider_kind: None,
                    provider_item_id: None,
                    token_estimate: None,
                }],
                run_config: run_config(),
            },
            max_steps: Some(32),
        })
        .await
        .expect("drive live MCP run");

    assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
    assert_eq!(
        outcome.state.runs.completed[0].status,
        RunStatus::Completed,
        "{}",
        run_failure_text(blobs.as_ref(), &outcome.state).await
    );
    assert!(
        !outcome
            .emitted_entries
            .iter()
            .any(|entry| matches!(entry.event.kind, CoreAgentEventKind::Tool(_))),
        "direct remote MCP must not create Lightspeed tool events"
    );

    let mcp_calls = mcp_call_items(blobs.as_ref(), &outcome.emitted_entries).await;
    assert!(
        mcp_calls
            .iter()
            .any(|item| item.to_string().contains(MCP_ECHO_MARKER)),
        "expected OpenAI mcp_call output containing marker; calls={mcp_calls:?}"
    );
    let assistant = assistant_text(blobs.as_ref(), &outcome.emitted_entries).await;
    assert!(
        assistant.contains(&format!("ECHO={MCP_ECHO_MARKER}")),
        "assistant did not echo marker; assistant={assistant:?}"
    );
}

fn remote_mcp_echo_tools() -> BTreeMap<ToolName, ToolSpec> {
    let tool = ToolSpec {
        name: ToolName::new("mcp_echo"),
        kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
            server_label: "echo".to_string(),
            server_url: MCP_ECHO_SERVER_URL.to_string(),
            description_ref: None,
            allowed_tools: Some(vec!["echo".to_string()]),
            approval: RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
            auth_ref: None,
        }),
        parallelism: ToolParallelism::ParallelSafe,
        target_requirement: ToolTargetRequirement::None,
    };
    BTreeMap::from([(tool.name.clone(), tool)])
}

fn session_config(model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        run: run_config(),
        turn: engine::TurnConfig {
            max_output_tokens: Some(1024),
            tool_choice: None,
            provider_params: Some(support::openai_params(
                &llm_runtime::OpenAiResponsesParams {
                    store: Some(false),
                    ..llm_runtime::OpenAiResponsesParams::default()
                },
            )),
        },
        context: ContextConfig { compaction: None },
        tools: Default::default(),
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(2),
        max_tool_rounds: Some(1),
        model_override: None,
        max_output_tokens: None,
        provider_params: None,
        tool_choice: None,
    }
}

async fn assistant_text(blobs: &dyn BlobStore, entries: &[engine::CoreAgentEntry]) -> String {
    let mut text = String::new();
    for entry in entries {
        if let CoreAgentEventKind::Context(engine::ContextEvent::EntriesApplied {
            entries, ..
        }) = &entry.event.kind
        {
            for item in entries {
                if matches!(
                    item.kind,
                    engine::ContextEntryKind::Message {
                        role: engine::ContextMessageRole::Assistant
                    }
                ) {
                    text.push_str(
                        &blobs
                            .read_text(&item.content_ref)
                            .await
                            .expect("assistant text"),
                    );
                    text.push('\n');
                }
            }
        }
    }
    text
}

async fn mcp_call_items(blobs: &dyn BlobStore, entries: &[engine::CoreAgentEntry]) -> Vec<Value> {
    let mut items = Vec::new();
    for entry in entries {
        if let CoreAgentEventKind::Context(engine::ContextEvent::EntriesApplied {
            entries, ..
        }) = &entry.event.kind
        {
            for item in entries {
                if item.provider_kind.as_deref()
                    == Some(engine::OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND)
                {
                    let bytes = blobs
                        .read_bytes(&item.content_ref)
                        .await
                        .expect("MCP call context bytes");
                    items.push(serde_json::from_slice(&bytes).expect("MCP call JSON"));
                }
            }
        }
    }
    items
}

async fn run_failure_text(blobs: &dyn BlobStore, state: &engine::CoreAgentState) -> String {
    let Some(run) = state.runs.completed.first() else {
        return "run did not complete".to_owned();
    };
    let Some(failure) = run.failure.as_ref() else {
        return format!("run status was {:?}", run.status);
    };
    let Some(message_ref) = failure.message_ref.as_ref() else {
        return format!("run failed without message: {:?}", failure.kind);
    };
    blobs
        .read_text(message_ref)
        .await
        .unwrap_or_else(|error| format!("failed to read failure message {message_ref}: {error}"))
}
