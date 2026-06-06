use std::{path::PathBuf, sync::Arc};

use engine::{
    AgentHandle, BlobRef, CompactionPolicy, ContextCompactionStatus, ContextCompactionTrigger,
    ContextConfig, ContextEntryInput, ContextEntryKey, ContextEntryKind, ContextMessageRole,
    ContextRemovalReason, CoreAgentCommand, CoreAgentEventKind, ModelProviderOptions,
    ModelSelection, OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND, OpenAiResponsesRequestDefaults,
    ProviderApiKind, ProviderRequestDefaults, RunConfig, RunStatus, SessionConfig, SessionId,
    TokenEstimate, TokenEstimateQuality, TurnConfig,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_clients::openai::responses::{Client, Config};
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};

mod support;

use support::retrying_openai_responses_client;

const LIVE_MARKER: &str = "FORGE-COMPACTION-LIVE-87421";

fn live_compaction_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_COMPACTION_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_RESPONSES_MODEL"))
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("OPENAI_API_KEY").expect(
        "OPENAI_API_KEY must be set in env or root .env to run llm-runtime compaction live tests",
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
#[ignore = "requires OPENAI_API_KEY and a compaction-capable OpenAI Responses model (costs real money)"]
async fn openai_responses_live_engine_prunes_and_reuses_provider_compaction() {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    let session_id = SessionId::new("session-live-compaction-engine");
    sessions
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("forge.live-compaction"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let model = ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_string(),
        model: live_compaction_model(),
        options: ModelProviderOptions::None,
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
    let runner = SessionRunner::new(RunnerStores::new(sessions, blobs.clone()), llm);

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: session_config(model),
            },
            max_steps: Some(64),
        })
        .await
        .expect("open session");

    let first_input_ref = blobs
        .put_bytes(first_prompt().into_bytes())
        .await
        .expect("store first prompt");
    let first = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 20,
            command: CoreAgentCommand::RequestRun {
                submission_id: None,
                input: user_input(first_input_ref.clone()),
                run_config: run_config(),
            },
            max_steps: Some(128),
        })
        .await
        .expect("drive first live run");

    assert_eq!(first.quiescence, RunnerQuiescence::Idle);
    assert_eq!(
        first.state.runs.completed[0].status,
        RunStatus::Completed,
        "{}",
        run_failure_text(blobs.as_ref(), &first.state).await
    );
    assert!(
        first.emitted_entries.iter().any(|entry| matches!(
            &entry.event.kind,
            CoreAgentEventKind::Context(engine::ContextEvent::EntriesRemoved {
                reason: ContextRemovalReason::ProviderCompacted,
                ..
            })
        )),
        "expected provider-compacted context removal event"
    );
    assert!(
        !first
            .state
            .context
            .entries
            .iter()
            .any(|entry| entry.content_ref == first_input_ref),
        "pre-compaction input should be pruned from active context"
    );
    assert_eq!(
        provider_compaction_entries(&first.state).len(),
        1,
        "latest active context should retain exactly one OpenAI compaction item"
    );

    let second_input_ref = blobs
        .put_bytes(
            b"What exact live marker was preserved earlier? Reply with only the marker.".to_vec(),
        )
        .await
        .expect("store second prompt");
    let second = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 30,
            command: CoreAgentCommand::RequestRun {
                submission_id: None,
                input: user_input(second_input_ref),
                run_config: run_config(),
            },
            max_steps: Some(128),
        })
        .await
        .expect("drive second live run");

    assert_eq!(second.quiescence, RunnerQuiescence::Idle);
    assert_eq!(
        second.state.runs.completed[1].status,
        RunStatus::Completed,
        "{}",
        run_failure_text(blobs.as_ref(), &second.state).await
    );
    let assistant_text = assistant_text(blobs.as_ref(), &second.emitted_entries).await;
    assert!(
        assistant_text.contains(LIVE_MARKER),
        "second response did not recover marker from compacted context; assistant={assistant_text:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and a compaction-capable OpenAI Responses model (costs real money)"]
async fn openai_responses_live_manual_standalone_compaction() {
    let session_id = SessionId::new("session-live-manual-standalone-compaction");
    let (runner, blobs) = live_runner(&session_id).await;

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: standalone_session_config(live_model_selection(), None),
            },
            max_steps: Some(64),
        })
        .await
        .expect("open session");

    let context_ref = store_openai_raw_message(
        blobs.as_ref(),
        "Summarize this short context for future continuation: Forge is testing manual standalone compaction.",
    )
    .await;
    let seed = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 20,
            command: CoreAgentCommand::UpsertContext {
                key: ContextEntryKey::new("client.openai.raw.manual"),
                entry: openai_raw_context_input(context_ref.clone(), None),
            },
            max_steps: Some(64),
        })
        .await
        .expect("seed context");

    assert!(seed.accepted);
    assert_eq!(seed.quiescence, RunnerQuiescence::Idle);
    assert!(
        provider_compaction_entries(&seed.state).is_empty(),
        "manual standalone compaction should not run before the explicit command"
    );

    let compacted = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 30,
            command: CoreAgentCommand::CompactContext,
            max_steps: Some(128),
        })
        .await
        .expect("manual compact context");

    assert_eq!(compacted.quiescence, RunnerQuiescence::Idle);
    assert!(
        has_compaction_requested(&compacted.emitted_entries, ContextCompactionTrigger::Manual),
        "expected manual compaction request event"
    );
    assert!(
        has_compaction_finished(
            &compacted.emitted_entries,
            ContextCompactionStatus::Succeeded
        ),
        "{}",
        compaction_failure_text(blobs.as_ref(), &compacted.emitted_entries).await
    );
    assert!(
        has_provider_compacted_removal(&compacted.emitted_entries),
        "expected provider-compacted prune after standalone compaction"
    );
    assert!(
        !active_context_contains_ref(&compacted.state, &context_ref),
        "pre-compaction context should be pruned from active context"
    );
    assert_eq!(
        provider_compaction_entries(&compacted.state).len(),
        1,
        "active context should retain exactly one OpenAI compaction item"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and a compaction-capable OpenAI Responses model (costs real money)"]
async fn openai_responses_live_high_watermark_standalone_compaction() {
    let session_id = SessionId::new("session-live-high-watermark-standalone-compaction");
    let (runner, blobs) = live_runner(&session_id).await;

    runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 10,
            command: CoreAgentCommand::OpenSession {
                config: standalone_session_config(live_model_selection(), Some(10)),
            },
            max_steps: Some(64),
        })
        .await
        .expect("open session");

    let context_ref = store_openai_raw_message(
        blobs.as_ref(),
        "Summarize this short context for future continuation: Forge is testing idle high-watermark standalone compaction.",
    )
    .await;
    let compacted = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 20,
            command: CoreAgentCommand::UpsertContext {
                key: ContextEntryKey::new("client.openai.raw.high_watermark"),
                entry: openai_raw_context_input(context_ref.clone(), Some(11)),
            },
            max_steps: Some(128),
        })
        .await
        .expect("seed context and compact at high watermark");

    assert_eq!(compacted.quiescence, RunnerQuiescence::Idle);
    assert!(
        has_compaction_requested(
            &compacted.emitted_entries,
            ContextCompactionTrigger::HighWatermark
        ),
        "expected high-watermark compaction request event"
    );
    assert!(
        has_compaction_finished(
            &compacted.emitted_entries,
            ContextCompactionStatus::Succeeded
        ),
        "{}",
        compaction_failure_text(blobs.as_ref(), &compacted.emitted_entries).await
    );
    assert!(
        has_provider_compacted_removal(&compacted.emitted_entries),
        "expected provider-compacted prune after high-watermark compaction"
    );
    assert!(
        !active_context_contains_ref(&compacted.state, &context_ref),
        "pre-compaction context should be pruned from active context"
    );
    assert_eq!(
        provider_compaction_entries(&compacted.state).len(),
        1,
        "active context should retain exactly one OpenAI compaction item"
    );
}

async fn live_runner(session_id: &SessionId) -> (SessionRunner, Arc<InMemoryBlobStore>) {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    sessions
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("forge.live-compaction"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let adapter = Arc::new(OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    ));
    let llm = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, adapter.clone())
            .with_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter),
    ));
    (
        SessionRunner::new(RunnerStores::new(sessions, blobs.clone()), llm),
        blobs,
    )
}

fn live_model_selection() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_string(),
        model: live_compaction_model(),
        options: ModelProviderOptions::None,
    }
}

fn session_config(model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        run: run_config(),
        turn: TurnConfig {
            max_output_tokens: Some(160),
            provider_request_defaults: ProviderRequestDefaults::OpenAiResponses(
                OpenAiResponsesRequestDefaults {
                    store: Some(false),
                    stream: Some(false),
                    ..OpenAiResponsesRequestDefaults::default()
                },
            ),
        },
        context: ContextConfig {
            compaction: Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens: Some(2_000),
            }),
        },
    }
}

fn standalone_session_config(
    model: ModelSelection,
    compact_threshold_tokens: Option<u32>,
) -> SessionConfig {
    SessionConfig {
        model,
        run: run_config(),
        turn: TurnConfig {
            max_output_tokens: Some(160),
            provider_request_defaults: ProviderRequestDefaults::OpenAiResponses(
                OpenAiResponsesRequestDefaults {
                    store: Some(false),
                    stream: Some(false),
                    ..OpenAiResponsesRequestDefaults::default()
                },
            ),
        },
        context: ContextConfig {
            compaction: Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens,
                target_tokens: Some(128),
            }),
        },
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(4),
        max_tool_rounds: Some(0),
        model_override: None,
        max_output_tokens: None,
        provider_request_defaults: None,
    }
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

async fn store_openai_raw_message(blobs: &dyn BlobStore, text: &str) -> BlobRef {
    let raw = serde_json::json!({
        "role": "user",
        "content": text,
    });
    blobs
        .put_bytes(serde_json::to_vec(&raw).expect("raw OpenAI message JSON"))
        .await
        .expect("store raw OpenAI message")
}

fn openai_raw_context_input(
    content_ref: BlobRef,
    token_estimate: Option<u32>,
) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content_ref,
        media_type: Some("application/json".to_owned()),
        preview: Some("OpenAI raw input message".to_owned()),
        provider_kind: Some("openai.responses.input_message".to_owned()),
        provider_item_id: None,
        token_estimate: token_estimate.map(|tokens| TokenEstimate {
            tokens,
            quality: TokenEstimateQuality::Estimated,
        }),
    }
}

fn first_prompt() -> String {
    let repeated = format!(
        "The exact live marker is {LIVE_MARKER}. Preserve this exact marker for a later question."
    );
    format!(
        "{}\n\nReply with exactly READY after preserving the marker.",
        std::iter::repeat(repeated.as_str())
            .take(260)
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn provider_compaction_entries(state: &engine::CoreAgentState) -> Vec<&engine::ContextEntry> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| {
            entry.provider_kind.as_deref() == Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        })
        .collect()
}

fn active_context_contains_ref(state: &engine::CoreAgentState, content_ref: &BlobRef) -> bool {
    state
        .context
        .entries
        .iter()
        .any(|entry| &entry.content_ref == content_ref)
}

fn has_compaction_requested(
    entries: &[engine::CoreAgentEntry],
    expected_trigger: ContextCompactionTrigger,
) -> bool {
    entries.iter().any(|entry| {
        matches!(
            &entry.event.kind,
            CoreAgentEventKind::Context(engine::ContextEvent::CompactionRequested {
                trigger,
                ..
            }) if *trigger == expected_trigger
        )
    })
}

fn has_compaction_finished(
    entries: &[engine::CoreAgentEntry],
    expected_status: ContextCompactionStatus,
) -> bool {
    entries.iter().any(|entry| {
        matches!(
            &entry.event.kind,
            CoreAgentEventKind::Context(engine::ContextEvent::CompactionFinished {
                status,
                ..
            }) if *status == expected_status
        )
    })
}

fn has_provider_compacted_removal(entries: &[engine::CoreAgentEntry]) -> bool {
    entries.iter().any(|entry| {
        matches!(
            &entry.event.kind,
            CoreAgentEventKind::Context(engine::ContextEvent::EntriesRemoved {
                reason: ContextRemovalReason::ProviderCompacted,
                ..
            })
        )
    })
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
                    ContextEntryKind::Message {
                        role: ContextMessageRole::Assistant
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

async fn compaction_failure_text(
    blobs: &dyn BlobStore,
    entries: &[engine::CoreAgentEntry],
) -> String {
    for entry in entries {
        if let CoreAgentEventKind::Context(engine::ContextEvent::CompactionFinished {
            status: ContextCompactionStatus::Failed,
            failure_ref: Some(failure_ref),
            ..
        }) = &entry.event.kind
        {
            return blobs
                .read_text(failure_ref)
                .await
                .unwrap_or_else(|error| format!("failed to read compaction failure: {error}"));
        }
    }
    "compaction did not finish with a failure ref".to_owned()
}

async fn run_failure_text(blobs: &dyn BlobStore, state: &engine::CoreAgentState) -> String {
    let Some(run) = state.runs.completed.last() else {
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
        .unwrap_or_else(|error| format!("failed to read failure message: {error}"))
}
