use std::sync::Arc;

use engine::{
    AgentHandle, BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole,
    CoreAdmitCommand, CoreAgentCodec, CoreAgentCommand, ModelProviderOptions, ModelSelection,
    ProviderApiKind, SessionId, SubmissionId, ToolProfileId,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};
use worker::{
    FAKE_TOOL_PROFILE_ID, FakeLlm, FakeTools, default_run_config, default_session_config,
    fake_tool_input_schema, fake_tool_registry,
};

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: "gpt-test".to_owned(),
        options: ModelProviderOptions::None,
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
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("forge.agent"),
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
    let (runner, session_id, blobs, _sessions) = runner().await;
    let schema_ref = blobs
        .put_bytes(fake_tool_input_schema())
        .await
        .expect("store schema");
    let config = default_session_config(model());

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
            command: CoreAgentCommand::SetToolRegistry {
                registry: fake_tool_registry(schema_ref),
            },
            max_steps: Some(64),
        })
        .await
        .expect("set registry");
    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 12,
            command: CoreAgentCommand::SelectToolProfile {
                profile_id: ToolProfileId::new(FAKE_TOOL_PROFILE_ID),
            },
            max_steps: Some(64),
        })
        .await
        .expect("select profile");

    let input_ref = blobs
        .put_bytes(b"hello".to_vec())
        .await
        .expect("store input");
    let outcome = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 13,
            command: CoreAgentCommand::RequestRun {
                submission_id: Some(SubmissionId::new("submit_test")),
                input: user_input(input_ref),
                run_config: default_run_config(),
            },
            max_steps: Some(64),
        })
        .await
        .expect("request run");

    assert_eq!(outcome.quiescence, RunnerQuiescence::Idle);
    let completed = outcome.state.runs.completed.last().expect("completed run");
    let output_ref = completed.output_ref.as_ref().expect("output ref");
    let output = blobs.read_text(output_ref).await.expect("read output");
    assert!(output.contains("Fake agent completed run"));
}

#[test]
fn core_command_admission_uses_core_agent_codec_shape() {
    use engine::CommandCodec;

    let codec = CoreAgentCodec;
    let command = CoreAgentCommand::RequestRun {
        submission_id: Some(SubmissionId::new("submit_test")),
        input: user_input(BlobRef::from_bytes(b"hello")),
        run_config: default_run_config(),
    };
    let dynamic = codec.encode_command(&command).expect("encode command");
    assert_eq!(dynamic.kind, "forge.core.command");
    assert_eq!(
        codec.decode_command(&dynamic).expect("decode command"),
        command
    );

    let _admitter = CoreAdmitCommand;
}

fn user_input(content_ref: BlobRef) -> Vec<ContextEntryInput> {
    vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: None,
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }]
}
