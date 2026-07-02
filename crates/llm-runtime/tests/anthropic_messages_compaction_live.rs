//! Live engine-loop compaction tests for the Anthropic Messages adapter.
//!
//! Anthropic has no provider-triggered compaction (the adapter rejects that
//! policy), so these cover the provider-standalone path: the engine plans a
//! compaction task, the adapter runs a summarization request, and the engine
//! prunes the compacted history in favor of the summary entry.

use std::{path::PathBuf, sync::Arc};

use engine::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND, AgentHandle, BlobRef, CompactionPolicy,
    ContextCompactionStatus, ContextCompactionTrigger, ContextConfig, ContextEntryInput,
    ContextEntryKey, ContextEntryKind, ContextMessageRole, ContextRemovalReason, CoreAgentCommand,
    CoreAgentEventKind, ModelSelection, ProviderApiKind, RunConfig, RunStatus, SessionConfig,
    SessionId, TokenEstimate, TokenEstimateQuality, TurnConfig,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_clients::anthropic::messages::{Client, Config};
use llm_runtime::{
    ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND, AnthropicMessagesLlmAdapter,
    LlmAdapterRegistry, LlmRuntime,
};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};

mod support;

use support::retrying_anthropic_messages_client;

const LIVE_MARKER: &str = "LIGHTSPEED-ANTHROPIC-COMPACTION-LIVE-4217";

fn live_model() -> String {
    env_or_dotenv_var("ANTHROPIC_MESSAGES_MODEL")
        .or_else(|_| env_or_dotenv_var("ANTHROPIC_LIVE_MODEL"))
        .unwrap_or_else(|_| "claude-opus-4-8".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("ANTHROPIC_API_KEY").expect(
        "ANTHROPIC_API_KEY must be set in env or root .env to run Anthropic compaction live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "ANTHROPIC_API_KEY is set but empty"
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("ANTHROPIC_BASE_URL") {
        config.base_url = base_url;
    }
    Client::new(config).expect("Anthropic Messages client")
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
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_manual_standalone_compaction_preserves_marker() {
    let session_id = SessionId::new("session-live-anthropic-manual-compaction");
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

    let context_ref = store_anthropic_raw_message(
        blobs.as_ref(),
        &format!(
            "Project kickoff notes: we are wiring the deployment pipeline this week. \
             The release codename for this rollout is {LIVE_MARKER}; the ops team uses it \
             to tag every artifact. We also decided to store session logs in Postgres."
        ),
    )
    .await;
    let seed = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 20,
            command: CoreAgentCommand::UpsertContext {
                key: ContextEntryKey::new("client.anthropic.raw.manual"),
                entry: anthropic_raw_context_input(context_ref.clone(), None),
            },
            max_steps: Some(64),
        })
        .await
        .expect("seed context");

    assert!(seed.accepted, "seed rejected: {:?}", seed.rejection);
    assert_eq!(seed.quiescence, RunnerQuiescence::Idle);
    assert!(
        compaction_summary_entries(&seed.state).is_empty(),
        "manual standalone compaction should not run before the explicit command"
    );

    let compacted = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
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
    let summaries = compaction_summary_entries(&compacted.state);
    assert_eq!(
        summaries.len(),
        1,
        "active context should retain exactly one Anthropic compaction summary"
    );
    let summary = blobs
        .read_text(&summaries[0].content_ref)
        .await
        .expect("summary text");
    assert!(
        summary.contains(LIVE_MARKER),
        "summary should preserve the marker; summary={summary:?}"
    );

    // Continue the session on the summary-only context: the model must still
    // recover the marker from the replacement entry.
    let question_ref = blobs
        .put_bytes(
            b"What exact live marker was preserved earlier? Reply with only the marker.".to_vec(),
        )
        .await
        .expect("store question");
    let recalled = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 40,
            command: CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                submission_id: None,
                source: engine::RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content_ref: question_ref,
                        media_type: None,
                        preview: None,
                        provider_kind: None,
                        provider_item_id: None,
                        token_estimate: None,
                    }],
                },
                run_config: run_config(),
            }),
            max_steps: Some(128),
        })
        .await
        .expect("drive recall run");

    assert_eq!(recalled.quiescence, RunnerQuiescence::Idle);
    assert_eq!(
        recalled.state.runs.completed[0].status,
        RunStatus::Completed,
        "{}",
        run_failure_text(blobs.as_ref(), &recalled.state).await
    );
    let assistant = assistant_text(blobs.as_ref(), &recalled.emitted_entries).await;
    assert!(
        assistant.contains(LIVE_MARKER),
        "post-compaction run did not recover the marker; assistant={assistant:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_high_watermark_standalone_compaction() {
    let session_id = SessionId::new("session-live-anthropic-high-watermark-compaction");
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

    let context_ref = store_anthropic_raw_message(
        blobs.as_ref(),
        "Summarize this short context for future continuation: Lightspeed is testing idle \
         high-watermark standalone compaction on the Anthropic Messages adapter.",
    )
    .await;
    let compacted = runner
        .drive_command(DriveCommand {
            session_id,
            observed_at_ms: 20,
            command: CoreAgentCommand::UpsertContext {
                key: ContextEntryKey::new("client.anthropic.raw.high_watermark"),
                entry: anthropic_raw_context_input(context_ref.clone(), Some(11)),
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
        compaction_summary_entries(&compacted.state).len(),
        1,
        "active context should retain exactly one Anthropic compaction summary"
    );
}

async fn live_runner(session_id: &SessionId) -> (SessionRunner, Arc<InMemoryBlobStore>) {
    let sessions = Arc::new(InMemorySessionStore::new());
    let blobs = Arc::new(InMemoryBlobStore::new());
    sessions
        .create_session(CreateSession {
            session_id: session_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.live-anthropic-compaction"),
            created_at_ms: 1,
        })
        .await
        .expect("create session");

    let adapter = Arc::new(AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    ));
    let llm = Arc::new(LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::AnthropicMessages, adapter.clone())
            .with_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter),
    ));
    (
        SessionRunner::new(RunnerStores::new(sessions, blobs.clone()), llm),
        blobs,
    )
}

fn live_model_selection() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::AnthropicMessages,
        provider_id: "anthropic".to_string(),
        model: live_model(),
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
            max_output_tokens: Some(256),
            tool_choice: None,
            provider_params: None,
        },
        context: ContextConfig {
            compaction: Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens,
                target_tokens: Some(256),
            }),
        },
        tools: Default::default(),
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(4),
        max_tool_rounds: Some(0),
        model_override: None,
        max_output_tokens: None,
        provider_params: None,
        tool_choice: None,
    }
}

async fn store_anthropic_raw_message(blobs: &dyn BlobStore, text: &str) -> BlobRef {
    let raw = serde_json::json!({
        "role": "user",
        "content": text,
    });
    blobs
        .put_bytes(serde_json::to_vec(&raw).expect("raw Anthropic message JSON"))
        .await
        .expect("store raw Anthropic message")
}

fn anthropic_raw_context_input(
    content_ref: BlobRef,
    token_estimate: Option<u32>,
) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::ProviderOpaque,
        content_ref,
        media_type: Some("application/json".to_owned()),
        preview: Some("Anthropic raw input message".to_owned()),
        provider_kind: Some(ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND.to_owned()),
        provider_item_id: None,
        token_estimate: token_estimate.map(|tokens| TokenEstimate {
            tokens,
            quality: TokenEstimateQuality::Estimated,
        }),
    }
}

fn compaction_summary_entries(state: &engine::CoreAgentState) -> Vec<&engine::ContextEntry> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| {
            entry.provider_kind.as_deref() == Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
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
