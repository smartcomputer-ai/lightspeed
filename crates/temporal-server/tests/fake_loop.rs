use std::{collections::BTreeMap, sync::Arc};

use engine::{
    BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentCommand,
    FunctionToolSpec, ModelSelection, ProviderApiKind, SessionId, SubmissionId, ToolChoice,
    ToolKind, ToolName, ToolParallelism, ToolSpec,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use temporal_server::worker::{
    FAKE_TOOL_NAME, FakeLlm, FakeTools, default_run_config, default_session_config,
};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: "gpt-test".to_owned(),
    }
}

async fn runner() -> (
    SessionRunner,
    SessionId,
    Arc<InMemoryBlobStore>,
    Arc<InMemorySessionStore>,
) {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let session_id = SessionId::new("session_test");
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
    let stores = RunnerStores::new(sessions.clone(), blobs.clone());
    let runner = SessionRunner::new(stores, Arc::new(FakeLlm::new(blobs.clone())))
        .with_tools(Arc::new(FakeTools::new(blobs.clone())));
    (runner, session_id, blobs, sessions)
}

#[tokio::test(flavor = "current_thread")]
async fn fake_llm_tool_loop_completes_a_run() {
    assert_tool_loop(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fake_llm_builtin_tool_loop_retains_identity_and_presentation() {
    assert_tool_loop(true).await;
}

async fn assert_tool_loop(builtin: bool) {
    let (runner, session_id, blobs, _sessions) = runner().await;
    let schema_ref = blobs
        .put_bytes(fake_tool_input_schema())
        .await
        .expect("store schema");
    let mut config = default_session_config(model());
    config.generation.tool_choice = Some(ToolChoice::Auto);
    config.generation.parallel_tool_use = Some(false);

    let opened = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession { config },
            max_steps: Some(64),
        })
        .await
        .expect("open session");
    assert!(opened.accepted);

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 11,
            command: CoreAgentCommand::ReplaceTools {
                expected_revision: Some(0),
                tools: if builtin {
                    let tool = tools::definitions::register(
                        "vfs.read_file",
                        Default::default(),
                        ToolParallelism::ParallelSafe,
                        Default::default(),
                    );
                    BTreeMap::from([(tool.name.clone(), tool)])
                } else {
                    fake_tool_set(schema_ref)
                },
            },
            max_steps: Some(64),
        })
        .await
        .expect("replace tools");

    let input_ref = blobs
        .put_bytes(b"hello".to_vec())
        .await
        .expect("store input");
    let outcome = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 13,
            command: CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                requested_by: None,
                notify_on_terminal: Vec::new(),
                submission_id: Some(SubmissionId::new("submit_test")),
                source: engine::RunRequestSource::Input {
                    input: user_input(input_ref),
                },
                run_config: default_run_config(),
            }),
            max_steps: Some(64),
        })
        .await
        .expect("request run");

    assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
    let completed = outcome.state.runs.completed.last().expect("completed run");
    let calls = outcome
        .emitted_entries
        .iter()
        .flat_map(|entry| match &entry.event {
            engine::CoreAgentEvent::Tool(engine::ToolEvent::BatchStarted { calls, .. }) => {
                calls.as_slice()
            }
            _ => &[],
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls.len(),
        1,
        "the fake must execute a tool before its final answer"
    );
    let (id, name) = if builtin {
        ("vfs.read_file", "vfs_read_file")
    } else {
        (FAKE_TOOL_NAME, FAKE_TOOL_NAME)
    };
    assert_eq!(calls[0].tool_id.as_ref().map(ToolName::as_str), Some(id));
    assert_eq!(calls[0].tool_name.as_str(), name);
    let content = completed.output.as_ref().expect("output content");
    let output = api_projection::project_content_text(blobs.as_ref(), content)
        .await
        .expect("project output")
        .expect("text output");
    assert!(output.contains("Fake agent completed run"));
}

fn fake_tool_input_schema() -> Vec<u8> {
    br#"{"type":"object","additionalProperties":false,"properties":{"text":{"type":"string"}},"required":["text"]}"#.to_vec()
}

fn fake_tool_set(input_schema_ref: BlobRef) -> BTreeMap<ToolName, ToolSpec> {
    let tool_name = ToolName::new(FAKE_TOOL_NAME);
    BTreeMap::from([(
        tool_name.clone(),
        ToolSpec {
            name: tool_name.clone(),
            execution: Default::default(),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: None,
                input_schema_ref,
                output_schema_ref: None,
                strict: Some(true),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        },
    )])
}

fn user_input(content_ref: BlobRef) -> Vec<ContextEntryInput> {
    vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: engine::ContentRef {
            content_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }]
}
