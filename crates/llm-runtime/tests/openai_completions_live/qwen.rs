//! Run with `cargo test -p llm-runtime --test openai_completions_live qwen:: -- --ignored --nocapture`.
//! `OPENROUTER_QWEN_PROVIDER` selects the pinned OpenRouter endpoint (default `parasail/bf16`).
//! For LiteLLM, set `QWEN_LIVE_BASE_URL` to its API base and `QWEN_LIVE_MODEL` to the deployment
//! name. Direct endpoints use `QWEN_LIVE_API_KEY` if supplied and otherwise use anonymous auth.

use super::{
    assistant_text, custom_model_provider, custom_provider_client, entry, env_or_dotenv_var,
    generation_request, openai_completions_params,
};
use harness::{
    ContextCompactionRequest, ContextCompactionStatus, ContextCompactionTask, ContextEntry,
    ContextEntryKey, ContextEntryKind, ContextEntrySource, ContextMessageRole,
    LlmGenerationRequest, LlmGenerationStatus, RunId, SteeringId, ToolCallId, ToolName, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_clients::{
    ApiResponse, EndpointOverride, LlmApiError, RequestAuth,
    openai::completions::{Completion, CreateCompletionRequest},
};
use llm_runtime::{
    LlmCompactionAdapter, LlmGenerationAdapter, OpenAiCompletionsApi, OpenAiCompletionsLlmAdapter,
    OpenAiCompletionsParams,
};
use serde_json::{Value, json};
use std::sync::Arc;

const MODEL: &str = "qwen/qwen3.5-9b";
// The provider ID selects Lightspeed's Compatible dialect for either destination.
const PROVIDER_ID: &str = "qwen-compatible";

async fn catalog(blobs: &InMemoryBlobStore, id: u64, skill: &str) -> ContextEntry {
    let mut catalog = entry(
        id,
        ContextEntryKind::Catalog {
            title: "Environment skills".to_owned(),
        },
        ContextEntrySource::ContextEdit,
        blobs
            .insert_text(format!("Available skill: {skill}."))
            .await,
    );
    catalog.key = Some(ContextEntryKey::new("environment.skills"));
    catalog
}

async fn question(blobs: &InMemoryBlobStore, id: u64, run: u64) -> ContextEntry {
    user_input(
        blobs,
        id,
        run,
        "Give only the name of the currently available skill.",
    )
    .await
}

async fn user_input(blobs: &InMemoryBlobStore, id: u64, run: u64, text: &str) -> ContextEntry {
    entry(
        id,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::RunInput {
            run_id: RunId::new(run),
            input_index: 0,
        },
        blobs.insert_text(text).await,
    )
}

async fn instruction(blobs: &InMemoryBlobStore, id: u64, key: &str, text: &str) -> ContextEntry {
    let mut layer = entry(
        id,
        ContextEntryKind::Instructions,
        ContextEntrySource::ContextEdit,
        blobs.insert_text(text).await,
    );
    layer.key = Some(ContextEntryKey::new(key));
    layer
}

// The compaction intent has no generation params. Pin the route at the transport
// boundary so compaction and generation reach the same upstream.
struct QwenLiveClient {
    upstream: Option<String>,
    client: Arc<dyn OpenAiCompletionsApi>,
}

#[async_trait::async_trait]
impl OpenAiCompletionsApi for QwenLiveClient {
    async fn create(
        &self,
        mut request: CreateCompletionRequest,
        auth: Option<RequestAuth<'_>>,
        endpoint: Option<&EndpointOverride>,
    ) -> Result<ApiResponse<Completion>, LlmApiError> {
        if let Some(upstream) = &self.upstream {
            request.extra.insert(
                "provider".into(),
                json!({"only":[upstream], "allow_fallbacks":false}),
            );
        }
        let route = self.upstream.as_deref().unwrap_or("direct endpoint");
        let roles: Vec<_> = request
            .messages
            .iter()
            .map(|message| message.role.as_str())
            .collect();
        eprintln!(
            "{} via {route}: {roles:?}, max_tokens={:?}",
            request.model, request.max_tokens
        );
        let response = self.client.create(request, auth, endpoint).await;
        match &response {
            Ok(response) => {
                let finish = response
                    .parsed
                    .choices
                    .first()
                    .and_then(|choice| choice.finish_reason.as_deref());
                let tokens = response
                    .parsed
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.completion_tokens);
                eprintln!("via {route}: finish={finish:?}, completion_tokens={tokens:?}");
            }
            Err(error) => {
                if let Some(rejection) = error.request_rejection()
                    && let Some(detail) = rejection
                        .raw_json
                        .as_ref()
                        .and_then(|body| body.pointer("/error/metadata/raw"))
                        .and_then(Value::as_str)
                {
                    eprintln!("HTTP {}: {detail}", rejection.status);
                }
            }
        }
        response
    }
}

fn adapter(blobs: Arc<InMemoryBlobStore>) -> OpenAiCompletionsLlmAdapter {
    let (providers, upstream) = match env_or_dotenv_var("QWEN_LIVE_BASE_URL") {
        Ok(base_url) => (
            custom_model_provider(
                PROVIDER_ID,
                &base_url,
                env_or_dotenv_var("QWEN_LIVE_API_KEY").ok(),
            ),
            None,
        ),
        Err(_) => {
            let key = env_or_dotenv_var("OPENROUTER_API_KEY")
                .expect("set OPENROUTER_API_KEY in env or root .env");
            let upstream = env_or_dotenv_var("OPENROUTER_QWEN_PROVIDER")
                .unwrap_or_else(|_| "parasail/bf16".into());
            (
                custom_model_provider(PROVIDER_ID, "https://openrouter.ai/api/v1", Some(key)),
                Some(upstream),
            )
        }
    };
    OpenAiCompletionsLlmAdapter::new(
        Arc::new(QwenLiveClient {
            upstream,
            client: custom_provider_client(),
        }),
        blobs,
    )
    .with_provider_key_resolver(Arc::new(providers))
}

fn qwen_request(entries: Vec<ContextEntry>, run: u64) -> LlmGenerationRequest {
    let mut request = generation_request(entries);
    request.run_id = RunId::new(run);
    request.request.model.provider_id = PROVIDER_ID.into();
    request.request.model.model =
        env_or_dotenv_var("QWEN_LIVE_MODEL").unwrap_or_else(|_| MODEL.into());
    request.request.output_limit = Some(2048);
    request.request.params = Some(openai_completions_params(&OpenAiCompletionsParams {
        temperature: Some(json!(0)),
        ..Default::default()
    }));
    request
}

async fn assert_answer(
    blobs: Arc<InMemoryBlobStore>,
    request: LlmGenerationRequest,
    expected: &[&str],
) {
    let execution = adapter(blobs.clone())
        .generate(request)
        .await
        .unwrap_or_else(|error| panic!("Qwen generation: {error}"));
    assert_eq!(
        execution.result.status,
        LlmGenerationStatus::Succeeded,
        "finish: {:?}",
        execution.result.facts.finish
    );
    let answer = assistant_text(&blobs, &execution).await.to_lowercase();
    for fact in expected {
        assert!(
            answer.contains(&fact.to_lowercase()),
            "expected {fact:?} in {answer:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn single_leading_catalog() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let entries = vec![
        catalog(&blobs, 1, "amber-quartz").await,
        question(&blobs, 2, 1).await,
    ];
    assert_answer(blobs, qwen_request(entries, 1), &["amber-quartz"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn leading_instruction_and_catalog() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let instructions =
        instruction(&blobs, 1, "instructions.000.default", "Answer concisely.").await;
    let entries = vec![
        instructions,
        catalog(&blobs, 2, "amber-quartz").await,
        question(&blobs, 3, 1).await,
    ];
    assert_answer(blobs, qwen_request(entries, 1), &["amber-quartz"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn catalog_refreshed_between_runs() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let original = catalog(&blobs, 1, "amber-quartz").await;
    let reply = entry(
        3,
        ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        },
        ContextEntrySource::AssistantOutput {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
        },
        blobs.insert_text("amber-quartz").await,
    );
    let mut updated = catalog(&blobs, 4, "violet-cedar").await;
    updated.supersedes = Some(original.entry_id);
    let entries = vec![
        original,
        question(&blobs, 2, 1).await,
        reply,
        updated,
        question(&blobs, 5, 2).await,
    ];
    assert_answer(blobs, qwen_request(entries, 2), &["violet-cedar"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn two_leading_instructions_without_a_catalog() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let entries = vec![
        instruction(
            &blobs,
            1,
            "instructions.000.default",
            "The release tag is MARBLE-73.",
        )
        .await,
        instruction(
            &blobs,
            2,
            "instructions.050.profile",
            "The project name is Copper Finch.",
        )
        .await,
        user_input(&blobs, 3, 1, "Give the release tag and the project name.").await,
    ];
    assert_answer(
        blobs,
        qwen_request(entries, 1),
        &["MARBLE-73", "Copper Finch"],
    )
    .await;
}

async fn tool_call(
    blobs: &InMemoryBlobStore,
    id: u64,
    call_id: &str,
    name: &str,
    args: Value,
) -> ContextEntry {
    let raw = json!({"id": call_id, "type": "function", "function": {"name": name, "arguments": args.to_string()}});
    let mut call = entry(
        id,
        ContextEntryKind::ToolCall {
            call_id: ToolCallId::try_new(call_id).expect("call id"),
            name: ToolName::try_new(name).expect("tool name"),
        },
        ContextEntrySource::AssistantOutput {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
        },
        llm_runtime::blob_io::put_json(blobs, &raw)
            .await
            .expect("store native call"),
    );
    call.content.provider_kind =
        Some(llm_runtime::openai_completions::OPENAI_COMPLETIONS_TOOL_CALL_PROVIDER_KIND.into());
    call.content.media_type = Some("application/json".into());
    call
}

async fn tool_result(
    blobs: &InMemoryBlobStore,
    id: u64,
    call_id: &str,
    text: &str,
) -> ContextEntry {
    entry(
        id,
        ContextEntryKind::ToolResult {
            call_id: ToolCallId::try_new(call_id).expect("call id"),
            is_error: false,
        },
        ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: Some(harness::ToolBatchId::new(1)),
        },
        blobs.insert_text(text).await,
    )
}

async fn steering(blobs: &InMemoryBlobStore, id: u64, text: &str) -> ContextEntry {
    entry(
        id,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::Steering {
            run_id: RunId::new(1),
            steering_id: SteeringId::new(1),
            input_index: 0,
        },
        blobs.insert_text(text).await,
    )
}

// The fixture is the planned snapshot sent to the adapter. Entry 5 is reserved
// for a catalog published during the batch and rendered after both results.
async fn parallel_tool_request(blobs: &InMemoryBlobStore) -> LlmGenerationRequest {
    let entries = vec![
        catalog(blobs, 1, "amber-quartz").await,
        user_input(blobs, 2, 1, "Report both temperatures from the tool results and the currently available skill name.").await,
        tool_call(blobs, 3, "call_zurich", "lookup_temperature", json!({"city":"Zurich"})).await,
        tool_call(blobs, 4, "call_bern", "lookup_temperature", json!({"city":"Bern"})).await,
        tool_result(blobs, 6, "call_zurich", "Zurich: 14.3 degrees Celsius").await,
        tool_result(blobs, 7, "call_bern", "Bern: 26.8 degrees Celsius").await,
    ];
    let mut request = qwen_request(entries, 1);
    request.turn_id = TurnId::new(2);
    request.request.tools = vec![super::weather_tool(blobs).await];
    request
}

async fn refreshed_tool_request(blobs: &InMemoryBlobStore) -> LlmGenerationRequest {
    let mut request = parallel_tool_request(blobs).await;
    let mut updated = catalog(blobs, 5, "violet-cedar").await;
    updated.supersedes = Some(request.request.context.entries[0].entry_id);
    request.request.context.entries.push(updated);
    request
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn parallel_tool_results() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let request = parallel_tool_request(&blobs).await;
    assert_answer(blobs, request, &["14.3", "26.8", "amber-quartz"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn parallel_tool_results_then_catalog() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let request = refreshed_tool_request(&blobs).await;
    assert_answer(blobs, request, &["14.3", "26.8", "violet-cedar"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn parallel_tool_results_then_catalog_and_steering() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let mut request = refreshed_tool_request(&blobs).await;
    request
        .request
        .context
        .entries
        .push(steering(&blobs, 8, "Also include ticket PINE-42 in your reply.").await);
    assert_answer(blobs, request, &["14.3", "26.8", "violet-cedar", "PINE-42"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an image-capable live Qwen endpoint (OpenRouter calls cost money)"]
async fn tool_image_and_steering_share_a_user_message() {
    use super::support::tool_media::{ToolMediaSet, tool_media_entries_for, view_images_tool_spec};
    let blobs = Arc::new(InMemoryBlobStore::new());
    let mut entries = vec![
        user_input(
            &blobs,
            1,
            1,
            "Report the dominant color of the image the tool shows.",
        )
        .await,
        tool_call(&blobs, 2, "call_image", "view_images", json!({})).await,
    ];
    let image = tool_media_entries_for(
        &blobs,
        ToolCallId::try_new("call_image").unwrap(),
        3,
        ToolMediaSet::SingleImage,
    )
    .await;
    entries.extend(image.entries);
    entries.push(steering(&blobs, 5, "Also include ticket LAKE-28.").await);
    let mut request = qwen_request(entries, 1);
    request.turn_id = TurnId::new(2);
    request.request.tools = vec![view_images_tool_spec(&blobs).await];
    assert_answer(blobs, request, &["red", "LAKE-28"]).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn compact_conversation_ending_in_user_input() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input = user_input(
        &blobs,
        2,
        1,
        "Remember the deployment tag MARBLE-73 for the next operator.",
    )
    .await;
    let covered_entry_ids = vec![input.entry_id];
    let request = qwen_request(
        vec![
            instruction(&blobs, 1, "instructions.000.default", "Answer concisely.").await,
            input,
        ],
        1,
    );
    let result = adapter(blobs.clone())
        .compact_context(ContextCompactionRequest {
            session_id: request.session_id,
            request: ContextCompactionTask {
                model: request.request.model,
                context: request.request.context,
                request_fingerprint: "qwen-compact-live".into(),
                target_tokens: Some(2048),
                covered_entry_ids,
                tools: vec![],
                input_limit_tokens: None,
            },
        })
        .await
        .unwrap_or_else(|error| panic!("Qwen compaction: {error}"));
    assert_eq!(result.status, ContextCompactionStatus::Succeeded);
    let [summary] = result.context_entries.as_slice() else {
        panic!("expected a single summary entry")
    };
    let text = blobs
        .read_text(&summary.content.content_ref)
        .await
        .expect("stored summary");
    assert!(
        text.contains("MARBLE-73"),
        "expected deployment tag in {text:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a live Qwen endpoint (OpenRouter calls cost money)"]
async fn first_turn_after_compaction_with_surviving_catalog() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    // Seed a completed summary so this case can run even when compaction itself fails.
    let mut summary = entry(
        4,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::Runtime {
            label: harness::STANDALONE_COMPACTION_SOURCE.into(),
        },
        blobs.insert_text("The deployment tag is MARBLE-73.").await,
    );
    summary.content.media_type = Some("text/plain".into());
    summary.content.provider_kind =
        Some(harness::OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND.into());
    let entries = vec![
        instruction(&blobs, 1, "instructions.000.default", "Answer concisely.").await,
        summary,
        catalog(&blobs, 2, "violet-cedar").await,
        user_input(
            &blobs,
            5,
            2,
            "Give the deployment tag and the available skill.",
        )
        .await,
    ];
    assert_answer(
        blobs,
        qwen_request(entries, 2),
        &["MARBLE-73", "violet-cedar"],
    )
    .await;
}
