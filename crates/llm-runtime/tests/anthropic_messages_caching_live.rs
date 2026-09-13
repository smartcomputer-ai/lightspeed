//! Prompt caching on Anthropic Messages, proven against the live API: the
//! adapter's breakpoints make the second request read its prefix from the
//! cache, and the ordinary things that happen to a session — a tool round
//! trip, a catalog update — keep the hit.

use std::sync::Arc;

use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    ContextEntrySource, ContextMessageRole, ContextSnapshot, LlmGenerationRequest,
    LlmGenerationStatus, LlmRequest, LlmUsage, ModelSelection, ProviderApiKind, RunId, SessionId,
    ToolChoice, ToolName, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmGenerationAdapter, params::AnthropicMessagesParams,
};
use serde_json::json;

mod support;

use support::{
    anthropic_messages_live_client as live_client, anthropic_messages_live_model as live_model,
};

use support::{
    anthropic_params,
    caching::{assert_cached_share, long_instructions},
    retrying_anthropic_messages_client,
};

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
            title: "Warehouse delivery schedule".to_string(),
        },
        ContextEntrySource::ContextEdit,
        content_ref,
    );
    entry.key = Some(ContextEntryKey::new("warehouse:delivery_schedule"));
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

fn intent_request(
    entries: Vec<ContextEntry>,
    params: Option<AnthropicMessagesParams>,
) -> LlmRequest {
    LlmRequest {
        model: ModelSelection {
            api_kind: ProviderApiKind::AnthropicMessages,
            provider_id: "anthropic".to_string(),
            model: live_model(),
        },
        request_fingerprint: "live-anthropic-caching".to_string(),
        context: ContextSnapshot {
            api_kind: ProviderApiKind::AnthropicMessages,
            context_revision: 0,
            entries,
            token_estimate: None,
        },
        tools: Vec::new(),
        tool_choice: None,
        output_limit: Some(2048),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
        provider_response_id: None,
        compaction: None,
        params: params.as_ref().map(anthropic_params),
    }
}

fn generation_request(turn_id: u64, request: LlmRequest) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("session-live-anthropic-caching"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(turn_id),
        request,
    }
}

fn adapter(blobs: Arc<InMemoryBlobStore>) -> AnthropicMessagesLlmAdapter {
    AnthropicMessagesLlmAdapter::new(retrying_anthropic_messages_client(live_client()), blobs)
}

async fn generate(
    adapter: &AnthropicMessagesLlmAdapter,
    blobs: &InMemoryBlobStore,
    turn_id: u64,
    request: LlmRequest,
) -> (LlmUsage, Vec<ContextEntryInput>) {
    let execution = adapter
        .generate(generation_request(turn_id, request))
        .await
        .expect("generate");
    assert_succeeded(blobs, &execution.result).await;
    let usage = execution
        .result
        .facts
        .usage
        .clone()
        .expect("usage reported");
    (usage, execution.result.context_entries)
}

async fn assert_succeeded(blobs: &InMemoryBlobStore, result: &engine::LlmGenerationResult) {
    let failure = match result.failure_ref.as_ref() {
        Some(reference) => blobs.read_text(reference).await.expect("failure details"),
        None => format!("finish={:?}", result.facts.finish),
    };
    assert_eq!(result.status, LlmGenerationStatus::Succeeded, "{failure}");
}

fn cached(usage: &LlmUsage) -> u32 {
    usage.cached_input_tokens.unwrap_or(0)
}

fn written(usage: &LlmUsage) -> u32 {
    usage.cache_write_input_tokens.unwrap_or(0)
}

fn prompt(usage: &LlmUsage) -> u32 {
    usage.input_tokens.expect("prompt tokens")
}

/// The first request writes the prefix; the second reads it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_caching_live_second_turn_reads_the_prefix() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let first_ref = text_blob(
        &blobs,
        "Please summarize the stock record for warehouse bay 17, including the item name, quantity, and inspection status.",
    )
    .await;

    let (first, retained) = generate(
        &adapter,
        &blobs,
        1,
        intent_request(
            vec![
                instructions_entry(1, instructions_ref.clone()),
                user_entry(2, first_ref.clone()),
            ],
            None,
        ),
    )
    .await;
    assert!(
        written(&first) > 0 || cached(&first) > 0,
        "the first request must write (or already read) the prefix, got {first:?}"
    );

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
        text_blob(
            &blobs,
            "Give the same warehouse inventory summary for bay 18.",
        )
        .await,
    ));
    let (second, _) = generate(&adapter, &blobs, 2, intent_request(entries, None)).await;
    assert_cached_share("second turn", cached(&second), prompt(&first));
}

/// A tool call and its result are appended after the cached prefix; the
/// follow-up request still reads the prefix.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_caching_live_tool_round_trip_keeps_the_prefix_warm() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let input_ref = text_blob(
        &blobs,
        "What is the current temperature in Zurich? Use the get_weather tool.",
    )
    .await;
    let schema_ref = blobs
        .put_bytes(
            serde_json::to_vec(&json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }))
            .expect("schema bytes"),
        )
        .await
        .expect("schema blob");
    let description_ref = text_blob(&blobs, "Get current weather for a city").await;
    let tool = engine::ToolSpec {
        name: ToolName::new("get_weather"),
        execution: Default::default(),
        kind: engine::ToolKind::Function(engine::FunctionToolSpec {
            description_ref: Some(description_ref),
            input_schema_ref: schema_ref,
            output_schema_ref: None,
            strict: None,
            provider_options_ref: None,
        }),
        parallelism: engine::ToolParallelism::ParallelSafe,
    };

    let mut request = intent_request(
        vec![
            instructions_entry(1, instructions_ref.clone()),
            user_entry(2, input_ref.clone()),
        ],
        None,
    );
    request.tools = vec![tool.clone()];
    request.tool_choice = Some(ToolChoice::RequiredAny);
    request.parallel_tool_use = Some(false);
    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("generate tool call");
    assert_succeeded(&blobs, &execution.result).await;
    let first = execution.result.facts.usage.clone().expect("usage");
    let tool_call = execution
        .result
        .facts
        .tool_calls
        .first()
        .expect("observed tool call")
        .clone();

    let mut entries = vec![
        instructions_entry(1, instructions_ref),
        user_entry(2, input_ref),
    ];
    entries.extend(
        execution
            .result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(3 + index as u64, item)),
    );
    let next_id = entries.len() as u64 + 1;
    let mut result_entry = entry(
        next_id,
        ContextEntryKind::ToolResult {
            call_id: tool_call.call_id.clone(),
            is_error: false,
        },
        ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: None,
        },
        text_blob(&blobs, "11°C and sunny").await,
    );
    result_entry.content.media_type = Some("text/plain".to_owned());
    entries.push(result_entry);
    let mut followup = intent_request(entries, None);
    followup.tools = vec![tool];
    let (second, _) = generate(&adapter, &blobs, 2, followup).await;
    assert_cached_share("after the tool round trip", cached(&second), prompt(&first));
}

/// A catalog update is appended and supersedes the earlier version, which
/// stays in place: the prefix through the earlier version is still read
/// from the cache.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_caching_live_superseded_catalog_keeps_the_prefix_warm() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let v1_ref = text_blob(
        &blobs,
        "The Zurich warehouse receives cardboard cartons from Monday through Friday.",
    )
    .await;
    let v2_ref = text_blob(
        &blobs,
        "The Zurich warehouse receives cardboard cartons from Monday through Friday. The Bern warehouse receives cardboard cartons on Mondays.",
    )
    .await;
    let first_ref = text_blob(&blobs, "Write a one-sentence delivery plan for sending cardboard cartons to our Zurich warehouse using the provided delivery schedule.").await;

    let (first, retained) = generate(
        &adapter,
        &blobs,
        1,
        intent_request(
            vec![
                instructions_entry(1, instructions_ref.clone()),
                catalog_entry(2, v1_ref.clone(), None),
                user_entry(3, first_ref.clone()),
            ],
            None,
        ),
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
        text_blob(
            &blobs,
            "Update the delivery plan to include the Bern warehouse from the revised schedule.",
        )
        .await,
    ));
    let (second, _) = generate(&adapter, &blobs, 2, intent_request(entries, None)).await;
    assert_cached_share("after the catalog update", cached(&second), prompt(&first));
}

/// The one-hour TTL is a params knob; the provider accepts it and writes
/// the prefix under it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_caching_live_one_hour_ttl_is_accepted() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let params = AnthropicMessagesParams {
        prompt_cache_ttl: Some("1h".to_string()),
        ..AnthropicMessagesParams::default()
    };
    let (first, _) = generate(
        &adapter,
        &blobs,
        1,
        intent_request(
            vec![
                instructions_entry(1, instructions_ref),
                user_entry(2, text_blob(&blobs, "Please summarize the stock record for warehouse bay 5, including the item name, quantity, and inspection status.").await),
            ],
            Some(params),
        ),
    )
    .await;
    assert!(
        written(&first) > 0 || cached(&first) > 0,
        "expected a cache write under the 1h TTL, got {first:?}"
    );
}
