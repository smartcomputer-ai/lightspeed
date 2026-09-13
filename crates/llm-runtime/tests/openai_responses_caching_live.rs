//! Prompt caching on OpenAI Responses, proven against the live API: with
//! the session id as `prompt_cache_key`, the second request reports most of
//! the previous prompt as cached, and a superseded catalog keeps the hit.

use std::sync::Arc;
use std::time::Duration;

use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    ContextEntrySource, ContextMessageRole, ContextSnapshot, LlmGenerationRequest,
    LlmGenerationStatus, LlmRequest, LlmUsage, ModelSelection, ProviderApiKind, RunId, SessionId,
    TurnId, storage::InMemoryBlobStore,
};
use llm_runtime::{LlmGenerationAdapter, OpenAiResponsesLlmAdapter, OpenAiResponsesParams};

mod support;

use support::{
    openai_responses_live_client as live_client, openai_responses_live_model as live_model,
};

use support::{
    caching::{MIN_CACHED_SHARE, assert_cached_share, long_instructions},
    openai_params, retrying_openai_responses_client,
};

/// OpenAI's cache is eventually consistent across its fleet; a read that
/// misses right after the write is retried a few times before failing.
const CACHE_READ_ATTEMPTS: usize = 3;

async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
    blobs.insert_text(text).await
}

fn entry(
    id: u64,
    kind: ContextEntryKind,
    source: ContextEntrySource,
    content_ref: BlobRef,
) -> ContextEntry {
    ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(id),
        kind,
        source,
        content: engine::ContentRef {
            content_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }
}

fn instructions_entry(id: u64, content_ref: BlobRef) -> ContextEntry {
    let mut entry = entry(
        id,
        ContextEntryKind::Instructions,
        ContextEntrySource::ContextEdit,
        content_ref,
    );
    entry.key = Some(ContextEntryKey::new("instructions.000.live"));
    entry
}

fn user_entry(id: u64, content_ref: BlobRef) -> ContextEntry {
    entry(
        id,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        content_ref,
    )
}

fn catalog_entry(id: u64, content_ref: BlobRef, supersedes: Option<u64>) -> ContextEntry {
    let mut entry = entry(
        id,
        ContextEntryKind::Catalog {
            title: "Bot directory".to_string(),
        },
        ContextEntrySource::ContextEdit,
        content_ref,
    );
    entry.key = Some(ContextEntryKey::new("bot:directory"));
    entry.supersedes = supersedes.map(ContextEntryId::new);
    entry
}

fn retained_context_entry(id: u64, item: &ContextEntryInput) -> ContextEntry {
    let mut retained = entry(
        id,
        item.kind.clone(),
        match item.kind {
            ContextEntryKind::ReasoningState => ContextEntrySource::Reasoning {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
            },
            _ => ContextEntrySource::AssistantOutput {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
            },
        },
        item.content.content_ref.clone(),
    );
    retained.content.media_type = item.content.media_type.clone();
    retained.preview = item.preview.clone();
    retained.content.provider_kind = item.content.provider_kind.clone();
    retained.provenance_ref = item.provenance_ref.clone();
    retained.token_estimate = item.token_estimate.clone();
    retained
}

fn intent_request(entries: Vec<ContextEntry>) -> LlmRequest {
    LlmRequest {
        model: ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_string(),
            model: live_model(),
        },
        request_fingerprint: "live-openai-responses-caching".to_string(),
        context: ContextSnapshot {
            api_kind: ProviderApiKind::OpenAiResponses,
            context_revision: 0,
            entries,
            token_estimate: None,
        },
        tools: Vec::new(),
        tool_choice: None,
        output_limit: Some(256),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
        provider_response_id: None,
        compaction: None,
        params: Some(openai_params(&OpenAiResponsesParams {
            store: Some(false),
            stream: Some(false),
            ..OpenAiResponsesParams::default()
        })),
    }
}

fn generation_request(turn_id: u64, request: LlmRequest) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("session-live-openai-responses-caching"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(turn_id),
        request,
    }
}

async fn generate(
    adapter: &OpenAiResponsesLlmAdapter,
    turn_id: u64,
    request: LlmRequest,
) -> (LlmUsage, Vec<ContextEntryInput>) {
    let execution = adapter
        .generate(generation_request(turn_id, request))
        .await
        .expect("generate");
    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    let usage = execution
        .result
        .facts
        .usage
        .clone()
        .expect("usage reported");
    (usage, execution.result.context_entries)
}

/// Retry the cache read a few times: the write from the previous request
/// may not have propagated yet.
async fn generate_until_cached(
    adapter: &OpenAiResponsesLlmAdapter,
    turn_id: u64,
    request: LlmRequest,
    previous_input: u32,
) -> LlmUsage {
    let mut last = None;
    for attempt in 1..=CACHE_READ_ATTEMPTS {
        let (usage, _) = generate(adapter, turn_id, request.clone()).await;
        let share =
            f64::from(usage.cached_input_tokens.unwrap_or(0)) / f64::from(previous_input.max(1));
        if share >= MIN_CACHED_SHARE {
            return usage;
        }
        eprintln!(
            "attempt {attempt}: cached share {:.0}%, retrying",
            share * 100.0
        );
        last = Some(usage);
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    last.expect("at least one attempt")
}

fn prompt(usage: &LlmUsage) -> u32 {
    usage.input_tokens.expect("prompt tokens")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_caching_live_second_turn_reads_the_prefix() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    );
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let first_ref = text_blob(
        &blobs,
        "Item 17 needs a status line. Reply in one sentence.",
    )
    .await;

    let (first, retained) = generate(
        &adapter,
        1,
        intent_request(vec![
            instructions_entry(1, instructions_ref.clone()),
            user_entry(2, first_ref.clone()),
        ]),
    )
    .await;

    let mut entries = vec![
        instructions_entry(1, instructions_ref),
        user_entry(2, first_ref),
    ];
    entries.extend(
        retained
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(3 + index as u64, item)),
    );
    let next_id = entries.len() as u64 + 1;
    entries.push(user_entry(
        next_id,
        text_blob(&blobs, "Now item 18. One sentence.").await,
    ));
    let second = generate_until_cached(&adapter, 2, intent_request(entries), prompt(&first)).await;
    assert_cached_share(
        "second turn",
        second.cached_input_tokens.unwrap_or(0),
        prompt(&first),
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_caching_live_superseded_catalog_keeps_the_prefix_warm() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    );
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let v1_ref = text_blob(&blobs, "- infra: accepts events addressed by you").await;
    let v2_ref = text_blob(
        &blobs,
        "- infra: accepts events addressed by you\n- comms: subscribes to what you publish",
    )
    .await;
    let first_ref = text_blob(&blobs, "Who can you reach? One sentence.").await;

    let (first, retained) = generate(
        &adapter,
        1,
        intent_request(vec![
            instructions_entry(1, instructions_ref.clone()),
            catalog_entry(2, v1_ref.clone(), None),
            user_entry(3, first_ref.clone()),
        ]),
    )
    .await;

    let mut entries = vec![
        instructions_entry(1, instructions_ref),
        catalog_entry(2, v1_ref, None),
        user_entry(3, first_ref),
    ];
    entries.extend(
        retained
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(4 + index as u64, item)),
    );
    let next_id = entries.len() as u64 + 1;
    entries.push(catalog_entry(next_id, v2_ref, Some(2)));
    entries.push(user_entry(
        next_id + 1,
        text_blob(&blobs, "And now? One sentence.").await,
    ));
    let second = generate_until_cached(&adapter, 2, intent_request(entries), prompt(&first)).await;
    assert_cached_share(
        "after the catalog update",
        second.cached_input_tokens.unwrap_or(0),
        prompt(&first),
    );
}
