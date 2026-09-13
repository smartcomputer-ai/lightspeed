//! Live engine-loop compaction tests for the Anthropic Messages adapter.
//!
//! Anthropic has no provider-triggered compaction (the adapter rejects that
//! policy), so these cover the provider-standalone path: the engine plans a
//! compaction task, the adapter runs a summarization request, and the engine
//! prunes the compacted history in favor of the summary entry.

use std::sync::Arc;

use engine::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND, BlobRef, CompactionPolicy,
    ContextCompactionStatus, ContextCompactionTrigger, ContextConfig, ContextEntryInput,
    ContextEntryKey, ContextEntryKind, ContextMessageRole, ContextRemovalReason, CoreAgentCommand,
    CoreAgentEvent, ModelSelection, ProviderApiKind, RunConfig, RunStatus, SessionConfig,
    SessionId, TokenEstimate, TokenEstimateQuality,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_runtime::{
    ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND, AnthropicMessagesLlmAdapter,
    LlmAdapterRegistry, LlmRuntime,
};
use test_support::{DriveCommand, RunnerQuiescence, RunnerStores, SessionRunner};

mod support;

use support::{
    anthropic_messages_live_client as live_client, anthropic_messages_live_model as live_model,
};

use support::retrying_anthropic_messages_client;

const LIVE_MARKER: &str = "LIGHTSPEED-ANTHROPIC-COMPACTION-LIVE-4217";

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

    // Keep this note short and plain. Claude Opus 5's real-time cyber
    // safeguard refused a three-sentence "Project kickoff notes: ..." version
    // of it (stop_reason `refusal`, category `cyber`, zero output) whatever
    // the subject — deployment pipeline or spring newsletter — while this
    // shape passes consistently; that is a classifier decision, not the
    // compaction behavior this test is about.
    let context_ref = store_anthropic_raw_message(
        blobs.as_ref(),
        &format!(
            "Remember this working title for the spring newsletter: {LIVE_MARKER}. \
             We will need it later in the project. We also decided to keep the meeting \
             notes in Postgres."
        ),
    )
    .await;
    let seed = runner
        .drive_command(DriveCommand {
            session_id: session_id.clone(),
            observed_at_ms: 20,
            command: CoreAgentCommand::UpsertContext {
                expected_revision: None,
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
        .read_text(&summaries[0].content.content_ref)
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
                notify_on_terminal: Vec::new(),
                submission_id: None,
                source: engine::RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: engine::ContentRef {
                            content_ref: question_ref,
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
                expected_revision: None,
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
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            origin: None,
            delete_after_close_ms: None,
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
        generation: engine::GenerationConfig {
            max_output_tokens: Some(2048),
            reasoning_effort: None,
            tool_choice: None,
            parallel_tool_use: None,
            processing_tier: None,
        },
        limits: Default::default(),
        context: ContextConfig {
            compaction: Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens,
                target_tokens: Some(256),
            }),
        },
        features: Default::default(),
    }
}

fn run_config() -> RunConfig {
    RunConfig {
        max_turns: Some(4),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
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
        content: engine::ContentRef {
            content_ref,
            media_type: Some("application/json".to_owned()),
            provider_kind: Some(ANTHROPIC_MESSAGES_INPUT_MESSAGE_PROVIDER_KIND.to_owned()),
        },
        preview: Some("Anthropic raw input message".to_owned()),
        origin: None,
        provenance_ref: None,
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
            entry.content.provider_kind.as_deref()
                == Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
        })
        .collect()
}

fn active_context_contains_ref(state: &engine::CoreAgentState, content_ref: &BlobRef) -> bool {
    state
        .context
        .entries
        .iter()
        .any(|entry| &entry.content.content_ref == content_ref)
}

fn has_compaction_requested(
    entries: &[engine::CoreAgentEntry],
    expected_trigger: ContextCompactionTrigger,
) -> bool {
    entries.iter().any(|entry| {
        matches!(
            &entry.event,
            CoreAgentEvent::Context(engine::ContextEvent::CompactionRequested {
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
            &entry.event,
            CoreAgentEvent::Context(engine::ContextEvent::CompactionFinished {
                status,
                ..
            }) if *status == expected_status
        )
    })
}

fn has_provider_compacted_removal(entries: &[engine::CoreAgentEntry]) -> bool {
    entries.iter().any(|entry| {
        matches!(
            &entry.event,
            CoreAgentEvent::Context(engine::ContextEvent::EntriesRemoved {
                reason: ContextRemovalReason::ProviderCompacted,
                ..
            })
        )
    })
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
                    ContextEntryKind::Message {
                        role: ContextMessageRole::Assistant
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

async fn compaction_failure_text(
    blobs: &dyn BlobStore,
    entries: &[engine::CoreAgentEntry],
) -> String {
    for entry in entries {
        if let CoreAgentEvent::Context(engine::ContextEvent::CompactionFinished {
            status: ContextCompactionStatus::Failed,
            failure_ref: Some(failure_ref),
            ..
        }) = &entry.event
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
