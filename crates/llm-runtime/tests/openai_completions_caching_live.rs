//! Prompt caching on OpenAI Chat Completions, proven against the live API:
//! with the session id as `prompt_cache_key`, the second request reports
//! most of the previous prompt as cached, a tool round trip keeps the hit,
//! and so does a refreshed catalog under layered instructions.
//!
//! `OPENAI_COMPLETIONS_BASE_URL` and `OPENAI_COMPLETIONS_PROVIDER_ID` point
//! the suite at a self-hosted server instead, for example vLLM started with
//! `--enable-prefix-caching --enable-prompt-tokens-details` and provider id
//! `vllm`. `OPENAI_COMPLETIONS_OUTPUT_LIMIT` gives a reasoning model room to
//! think, and `OPENAI_COMPLETIONS_EXTRA_BODY` adds server-specific fields.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use harness::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    ContextEntrySource, ContextMessageRole, ContextSnapshot, LlmGenerationRequest,
    LlmGenerationStatus, LlmRequest, LlmUsage, ModelSelection, ProviderApiKind, RunId, SessionId,
    ToolChoice, ToolId, ToolSpec, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_runtime::{
    LlmGenerationAdapter, OpenAiCompletionsLlmAdapter, OpenAiCompletionsParams, ResolvedEndpoint,
    ResolvedModelProvider, StaticModelProviders,
};
use serde_json::json;

mod support;

use support::{
    caching::{MIN_CACHED_SHARE, assert_cached_share, long_instructions},
    env_or_dotenv_var, openai_completions_live_client, openai_completions_live_model,
    openai_completions_live_provider_id, openai_completions_params,
    retrying_openai_completions_client,
};

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
        content: harness::ContentRef {
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
    layer_entry(id, "instructions.000.live", content_ref)
}

fn layer_entry(id: u64, key: &str, content_ref: BlobRef) -> ContextEntry {
    let mut entry = entry(
        id,
        ContextEntryKind::Instructions,
        ContextEntrySource::ContextEdit,
        content_ref,
    );
    entry.key = Some(ContextEntryKey::new(key));
    entry
}

fn catalog_entry(id: u64, content_ref: BlobRef, supersedes: Option<u64>) -> ContextEntry {
    let mut entry = entry(
        id,
        ContextEntryKind::Catalog {
            title: "Sub-agent catalog".to_owned(),
        },
        ContextEntrySource::ContextEdit,
        content_ref,
    );
    entry.supersedes = supersedes.map(ContextEntryId::new);
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
        code_mode: None,
        model: ModelSelection {
            api_kind: ProviderApiKind::OpenAiCompletions,
            provider_id: openai_completions_live_provider_id(),
            model: openai_completions_live_model(),
        },
        request_fingerprint: "live-openai-completions-caching".to_owned(),
        context: ContextSnapshot {
            api_kind: ProviderApiKind::OpenAiCompletions,
            context_revision: 0,
            entries,
            token_estimate: None,
        },
        tools: Vec::new(),
        tool_choice: None,
        output_limit: Some(output_limit()),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
        compaction: None,
        params: Some(openai_completions_params(&OpenAiCompletionsParams {
            store: Some(false),
            stream: Some(false),
            extra: extra_body(),
            ..Default::default()
        })),
    }
}

/// Reasoning models (Qwen 3.5 thinks first) need room beyond the answer:
/// `OPENAI_COMPLETIONS_OUTPUT_LIMIT` raises the default of 256.
fn output_limit() -> u32 {
    env_or_dotenv_var("OPENAI_COMPLETIONS_OUTPUT_LIMIT")
        .map(|limit| {
            limit
                .parse()
                .expect("OPENAI_COMPLETIONS_OUTPUT_LIMIT is a number")
        })
        .unwrap_or(256)
}

/// Server-specific request fields from `OPENAI_COMPLETIONS_EXTRA_BODY`, a
/// JSON object, for example `{"chat_template_kwargs":{"enable_thinking":false}}`
/// for a Qwen model on vLLM.
fn extra_body() -> BTreeMap<String, serde_json::Value> {
    env_or_dotenv_var("OPENAI_COMPLETIONS_EXTRA_BODY")
        .map(|body| {
            serde_json::from_str(&body).expect("OPENAI_COMPLETIONS_EXTRA_BODY is a JSON object")
        })
        .unwrap_or_default()
}

fn generation_request(turn_id: u64, request: LlmRequest) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("session-live-openai-completions-caching"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(turn_id),
        request,
    }
}

fn adapter(blobs: Arc<InMemoryBlobStore>) -> OpenAiCompletionsLlmAdapter {
    let adapter = OpenAiCompletionsLlmAdapter::new(
        retrying_openai_completions_client(openai_completions_live_client()),
        blobs,
    );
    let provider_id = openai_completions_live_provider_id();
    if provider_id == "openai" {
        return adapter;
    }
    // A custom provider, as a universe's model-provider record defines it:
    // the server's base URL, Chat Completions, no stored credential.
    let base_url = env_or_dotenv_var("OPENAI_COMPLETIONS_BASE_URL")
        .expect("OPENAI_COMPLETIONS_BASE_URL is required with a custom provider id");
    let endpoint = ResolvedEndpoint::new(
        &base_url,
        &BTreeMap::new(),
        ["openai:completions".to_owned()],
    )
    .expect("endpoint");
    adapter.with_provider_key_resolver(Arc::new(StaticModelProviders::new().with_provider(
        provider_id,
        ResolvedModelProvider {
            auth: None,
            endpoint: Some(endpoint),
        },
    )))
}

async fn generate(
    adapter: &OpenAiCompletionsLlmAdapter,
    turn_id: u64,
    request: LlmRequest,
) -> (
    LlmUsage,
    Vec<ContextEntryInput>,
    Vec<harness::ObservedToolCall>,
) {
    let execution = adapter
        .generate(generation_request(turn_id, request))
        .await
        .expect("generate");
    assert_eq!(
        execution.result.status,
        LlmGenerationStatus::Succeeded,
        "finish {:?}, usage {:?}",
        execution.result.facts.finish,
        execution.result.facts.usage
    );
    let usage = execution
        .result
        .facts
        .usage
        .clone()
        .expect("usage reported");
    (
        usage,
        execution.result.context_entries,
        execution.result.facts.tool_calls,
    )
}

async fn generate_until_cached(
    adapter: &OpenAiCompletionsLlmAdapter,
    turn_id: u64,
    request: LlmRequest,
    previous_input: u32,
) -> LlmUsage {
    let mut last = None;
    for attempt in 1..=CACHE_READ_ATTEMPTS {
        let (usage, _, _) = generate(adapter, turn_id, request.clone()).await;
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

async fn weather_tool(blobs: &InMemoryBlobStore) -> ToolSpec {
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
    let description_ref = text_blob(blobs, "Get current weather for a city").await;
    ToolSpec {
        id: ToolId::new("get_weather"),
        execution: Default::default(),
        kind: harness::ToolKind::Function(harness::FunctionToolSpec {
            description_ref: Some(description_ref),
            input_schema_ref: schema_ref,
            output_schema_ref: None,
            strict: None,
            provider_options_ref: None,
        }),
        parallelism: harness::ToolParallelism::ParallelSafe,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_caching_live_second_turn_reads_the_prefix() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let first_ref = text_blob(
        &blobs,
        "Item 17 needs a status line. Reply in one sentence.",
    )
    .await;

    let (first, retained, _) = generate(
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
async fn openai_completions_caching_live_tool_round_trip_keeps_the_prefix_warm() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let instructions_ref = text_blob(&blobs, &long_instructions()).await;
    let input_ref = text_blob(
        &blobs,
        "What is the current temperature in Zurich? Use the get_weather tool.",
    )
    .await;
    let tool = weather_tool(&blobs).await;

    let mut request = intent_request(vec![
        instructions_entry(1, instructions_ref.clone()),
        user_entry(2, input_ref.clone()),
    ]);
    request.tools = vec![tool.clone()];
    request.tool_choice = Some(ToolChoice::RequiredAny);
    let (first, retained, tool_calls) = generate(&adapter, 1, request).await;
    let tool_call = tool_calls.first().expect("observed tool call").clone();

    let mut entries = vec![
        instructions_entry(1, instructions_ref),
        user_entry(2, input_ref),
    ];
    entries.extend(
        retained
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
    let mut followup = intent_request(entries);
    followup.tools = vec![tool];
    let second = generate_until_cached(&adapter, 2, followup, prompt(&first)).await;
    assert_cached_share(
        "after the tool round trip",
        second.cached_input_tokens.unwrap_or(0),
        prompt(&first),
    );
}

/// The shape a real session sends: instructions in layers, a catalog before
/// the first message, and between runs a refreshed catalog at the tail. Chat
/// templates that accept only one leading system message (Qwen 3.5) reject
/// any other shape, and the refresh must not move the cached prefix.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money) or a self-hosted server"]
async fn openai_completions_caching_live_catalog_refresh_keeps_the_prefix_warm() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let adapter = adapter(blobs.clone());
    let layers = vec![
        layer_entry(
            1,
            "instructions.000.default",
            text_blob(&blobs, &long_instructions()).await,
        ),
        layer_entry(
            2,
            "instructions.050.profile",
            text_blob(&blobs, "You are the release assistant of a small team.").await,
        ),
        layer_entry(
            3,
            "instructions.110.environment",
            text_blob(&blobs, "Commands run in /data/workspace.").await,
        ),
    ];
    let catalog = catalog_entry(
        4,
        text_blob(
            &blobs,
            "- reviewer: reviews a diff\n- tester: runs the test suite",
        )
        .await,
        None,
    );
    let first_ref = text_blob(
        &blobs,
        "Item 17 needs a status line. Reply in one sentence.",
    )
    .await;

    let mut entries = layers;
    entries.push(catalog);
    entries.push(user_entry(5, first_ref));
    let (first, retained, _) = generate(&adapter, 1, intent_request(entries.clone())).await;

    entries.extend(
        retained
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(6 + index as u64, item)),
    );
    let next_id = entries.len() as u64 + 2;
    entries.push(catalog_entry(
        next_id,
        text_blob(
            &blobs,
            "- reviewer: reviews a diff\n- tester: runs the test suite\n- writer: drafts release notes",
        )
        .await,
        Some(4),
    ));
    entries.push(user_entry(
        next_id + 1,
        text_blob(&blobs, "Now item 18. One sentence.").await,
    ));
    let second = generate_until_cached(&adapter, 2, intent_request(entries), prompt(&first)).await;
    assert_cached_share(
        "after a catalog refresh",
        second.cached_input_tokens.unwrap_or(0),
        prompt(&first),
    );
}
