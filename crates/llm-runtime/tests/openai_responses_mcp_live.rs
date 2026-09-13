use std::{collections::BTreeMap, sync::Arc};

use engine::{
    ContextConfig, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentCommand,
    CoreAgentEvent, ModelSelection, ProviderApiKind, RemoteMcpApprovalPolicy, RemoteMcpToolSpec,
    RunConfig, RunStatus, SessionConfig, SessionId, ToolKind, ToolName, ToolParallelism, ToolSpec,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter};
use serde_json::Value;
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};

mod support;

use support::{
    openai_responses_live_client as live_client, openai_responses_live_model as live_model,
};

use support::retrying_openai_responses_client;

const MCP_TEST_SERVER_URL: &str = "https://mcpplaygroundonline.com/mcp-stateless-server";
const MCP_TEST_TOOL: &str = "which_protocol_era";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and public MCP server access (costs real money)"]
async fn openai_responses_live_core_session_uses_public_remote_mcp() {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let session_id = SessionId::new("session-live-mcp-playground");
    sessions
        .create_session(CreateSession {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
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
                tools: remote_mcp_tools(),
            },
            max_steps: None,
        })
        .await
        .expect("replace tools");

    let input_ref = blobs
        .put_bytes(
            format!(
                "Use the remote MCP server labeled playground. Call its {MCP_TEST_TOOL} tool \
                 with an empty JSON object. After the tool returns, reply exactly \
                 PROTOCOL_ERA=<the era returned by the tool>, substituting the value."
            )
            .into_bytes(),
        )
        .await
        .expect("write prompt");
    let outcome = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 20,
            command: CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                notify_on_terminal: Vec::new(),
                submission_id: None,
                source: engine::RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: engine::ContentRef {
                            content_ref: input_ref,
                            media_type: None,
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                },
                run_config: run_config(),
            }),
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
            .any(|entry| matches!(entry.event, CoreAgentEvent::Tool(_))),
        "direct remote MCP must not create Lightspeed tool events"
    );

    let mcp_calls = mcp_call_items(blobs.as_ref(), &outcome.emitted_entries).await;
    let era = protocol_era(&mcp_calls)
        .unwrap_or_else(|| panic!("expected OpenAI MCP protocol era; calls={mcp_calls:?}"));
    let assistant = assistant_text(blobs.as_ref(), &outcome.emitted_entries).await;
    assert!(
        assistant.contains(&format!("PROTOCOL_ERA={era}")),
        "assistant did not report protocol era; assistant={assistant:?}"
    );
}

fn protocol_era(items: &[Value]) -> Option<&'static str> {
    ["modern", "legacy"]
        .into_iter()
        .find(|era| items.iter().any(|item| item.to_string().contains(era)))
}

fn remote_mcp_tools() -> BTreeMap<ToolName, ToolSpec> {
    let tool = ToolSpec {
        name: ToolName::new("mcp_playground"),
        execution: Default::default(),
        kind: ToolKind::RemoteMcp(RemoteMcpToolSpec {
            server_id: "playground".to_string(),
            record_revision: 1,
            server_label: "playground".to_string(),
            server_url: MCP_TEST_SERVER_URL.to_string(),
            description_ref: None,
            allowed_tools: Some(vec![MCP_TEST_TOOL.to_string()]),
            execution: engine::RemoteMcpExecution::Provider,
            exposure: engine::RemoteMcpExposure::Inject,
            approval: RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
            auth_ref: None,
            auth_required: false,
            allow_private_network: false,
        }),
        parallelism: ToolParallelism::ParallelSafe,
    };
    BTreeMap::from([(tool.name.clone(), tool)])
}

fn session_config(model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        generation: engine::GenerationConfig {
            max_output_tokens: Some(1024),
            reasoning_effort: None,
            tool_choice: None,
            parallel_tool_use: None,
            processing_tier: None,
        },
        limits: Default::default(),
        context: ContextConfig { compaction: None },
        features: Default::default(),
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(2),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
        max_tool_rounds: Some(1),
        model_override: None,
        max_output_tokens: None,
        provider_params: Some(support::openai_params(
            &llm_runtime::OpenAiResponsesParams {
                store: Some(false),
                ..llm_runtime::OpenAiResponsesParams::default()
            },
        )),
        tool_choice: None,
    }
}

async fn assistant_text(blobs: &dyn BlobStore, entries: &[engine::CoreAgentEntry]) -> String {
    let mut text = String::new();
    for entry in entries {
        if let CoreAgentEvent::Context(engine::ContextEvent::EntriesApplied { entries, .. }) =
            &entry.event
        {
            for item in entries {
                if matches!(
                    item.kind,
                    engine::ContextEntryKind::Message {
                        role: engine::ContextMessageRole::Assistant
                    }
                ) {
                    text.push_str(&support::content_text(blobs, &item.content).await);
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
        if let CoreAgentEvent::Context(engine::ContextEvent::EntriesApplied { entries, .. }) =
            &entry.event
        {
            for item in entries {
                if item.content.provider_kind.as_deref()
                    == Some(engine::OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND)
                {
                    let bytes = blobs
                        .read_bytes(&item.content.content_ref)
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
