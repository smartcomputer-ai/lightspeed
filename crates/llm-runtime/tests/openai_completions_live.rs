use std::{collections::BTreeMap, sync::Arc};

use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ContextSnapshot, FunctionToolSpec, LlmFinish, LlmGenerationRequest,
    LlmGenerationStatus, LlmRequest, ModelSelection, ProviderApiKind, RunId, SessionId, ToolChoice,
    ToolExecutionSpec, ToolKind, ToolName, ToolParallelism, ToolSpec, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_runtime::{
    LlmAdapterError, LlmGenerationAdapter, OpenAiCompletionsLlmAdapter, OpenAiCompletionsParams,
    ResolvedEndpoint, ResolvedModelProvider, ResolvedProviderAuth, StaticModelProviders,
};
use serde_json::json;

mod support;

use support::{
    deepseek_completions_live_model, env_or_dotenv_var, openai_completions_live_client,
    openai_completions_live_model, openai_completions_params, retrying_openai_completions_client,
};

const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";

async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
    blobs.insert_text(text).await
}

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiCompletions,
        provider_id: "openai".to_owned(),
        model: openai_completions_live_model(),
    }
}

fn entry(
    id: u64,
    kind: ContextEntryKind,
    source: ContextEntrySource,
    content_ref: BlobRef,
) -> ContextEntry {
    ContextEntry {
        entry_id: ContextEntryId::new(id),
        key: None,
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

fn generation_request(entries: Vec<ContextEntry>) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("session-openai-completions-live"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: model(),
            request_fingerprint: "openai-completions-live".to_owned(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiCompletions,
                context_revision: 0,
                entries,
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(384),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            provider_response_id: None,
            compaction: None,
            params: Some(openai_completions_params(&OpenAiCompletionsParams {
                store: Some(false),
                stream: Some(false),
                ..Default::default()
            })),
        },
    }
}

fn live_adapter(blobs: Arc<InMemoryBlobStore>) -> OpenAiCompletionsLlmAdapter {
    OpenAiCompletionsLlmAdapter::new(
        retrying_openai_completions_client(openai_completions_live_client()),
        blobs,
    )
    .with_debug_dumps(true)
}

fn deepseek_generation_request(entries: Vec<ContextEntry>) -> LlmGenerationRequest {
    let mut request = generation_request(entries);
    request.request.model = ModelSelection {
        api_kind: ProviderApiKind::OpenAiCompletions,
        provider_id: "deepseek".to_owned(),
        model: deepseek_completions_live_model(),
    };
    request.request.output_limit = Some(512);
    request.request.reasoning_effort = Some("high".to_owned());
    request.request.params = None;
    request
}

fn deepseek_adapter(blobs: Arc<InMemoryBlobStore>) -> OpenAiCompletionsLlmAdapter {
    let api_key = env_or_dotenv_var("DEEPSEEK_API_KEY")
        .expect("DEEPSEEK_API_KEY must be set in env or root .env");
    let base_url = env_or_dotenv_var("DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com".to_owned());
    let endpoint = ResolvedEndpoint::new(
        &base_url,
        &BTreeMap::new(),
        ["openai:completions".to_owned()],
    )
    .expect("DeepSeek endpoint");
    let providers = StaticModelProviders::new().with_provider(
        "deepseek",
        ResolvedModelProvider {
            auth: Some(ResolvedProviderAuth::api_key(api_key)),
            endpoint: Some(endpoint),
        },
    );
    // The provider record must supply both the destination and credentials.
    let client = llm_clients::openai::completions::Client::new(
        llm_clients::openai::completions::Config::without_api_key(),
    )
    .expect("base OpenAI client");
    OpenAiCompletionsLlmAdapter::new(retrying_openai_completions_client(client), blobs)
        .with_provider_key_resolver(Arc::new(providers))
        .with_debug_dumps(true)
}

async fn assistant_text(
    blobs: &InMemoryBlobStore,
    execution: &llm_runtime::LlmGenerationExecution,
) -> String {
    let entry = execution
        .result
        .context_entries
        .iter()
        .find(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant
                }
            )
        })
        .expect("assistant message");
    assert_eq!(
        entry.content.media_type.as_deref(),
        Some("application/json")
    );
    let raw: serde_json::Value = serde_json::from_slice(
        &blobs
            .read_bytes(&entry.content.content_ref)
            .await
            .expect("native content"),
    )
    .unwrap();
    let text = support::content_text(blobs, &entry.content).await;
    assert_eq!(
        Some(text.clone()),
        llm_clients::content::openai_completion_message(&raw)
    );
    text
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and Fast mode access (costs real money)"]
async fn openai_completions_runtime_live_fast_mode_reports_effective_service_tier() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, "Reply with exactly: fast mode ok").await;
    let mut request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        input_ref,
    )]);
    request.request.model.model =
        env_or_dotenv_var("OPENAI_FAST_MODE_MODEL").unwrap_or_else(|_| "gpt-5.6-sol".to_owned());
    request.request.reasoning_effort = Some("none".to_owned());
    request.request.output_limit = Some(64);
    request.request.processing_tier = Some(engine::ModelProcessingTier::Fast);
    request.request.params = Some(openai_completions_params(&OpenAiCompletionsParams {
        store: Some(false),
        stream: Some(false),
        ..Default::default()
    }));

    let execution = live_adapter(blobs.clone())
        .generate(request)
        .await
        .expect("Fast mode completion");
    let provider_request: serde_json::Value = serde_json::from_str(
        &blobs
            .read_text(&dumps(&execution).provider_request_ref)
            .await
            .expect("provider request"),
    )
    .expect("provider request JSON");
    assert_eq!(provider_request["service_tier"], "fast");

    let raw_response: serde_json::Value = serde_json::from_str(
        &blobs
            .read_text(&dumps(&execution).raw_response_ref)
            .await
            .expect("raw response"),
    )
    .expect("raw response JSON");
    assert!(
        matches!(
            raw_response
                .get("service_tier")
                .and_then(serde_json::Value::as_str),
            Some("fast" | "priority")
        ),
        "expected Fast mode response tier, got {raw_response}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_text_instructions_reasoning_and_usage() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let instructions_ref =
        text_blob(&blobs, "Answer tersely and follow exact-output requests.").await;
    let user_ref = text_blob(&blobs, "Compute 37 * 19. Reply with only the integer.").await;
    let mut request = generation_request(vec![
        entry(
            1,
            ContextEntryKind::Instructions,
            ContextEntrySource::ContextEdit,
            instructions_ref,
        ),
        entry(
            2,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            user_ref,
        ),
    ]);
    request.request.reasoning_effort = Some("low".to_owned());

    let execution = live_adapter(blobs.clone())
        .generate(request)
        .await
        .expect("generate");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert!(assistant_text(&blobs, &execution).await.contains("703"));
    let usage = execution.result.facts.usage.expect("usage");
    assert!(usage.input_tokens.unwrap_or_default() > 0);
    assert!(usage.output_tokens.unwrap_or_default() > 0);
    assert_eq!(execution.result.facts.finish, LlmFinish::Stop);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_image_input() {
    use base64::Engine as _;

    let blobs = Arc::new(InMemoryBlobStore::new());
    let image_ref = blobs
        .put_bytes(
            base64::engine::general_purpose::STANDARD
                .decode(RED_PNG_BASE64)
                .expect("PNG"),
        )
        .await
        .expect("store image");
    let question_ref = text_blob(
        &blobs,
        "Name the dominant image color with one lowercase English word.",
    )
    .await;
    let source = ContextEntrySource::RunInput {
        run_id: RunId::new(1),
        input_index: 0,
    };
    let mut image = entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source.clone(),
        image_ref,
    );
    image.content.media_type = Some("image/png".to_owned());
    let request = generation_request(vec![
        image,
        entry(
            2,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source,
            question_ref,
        ),
    ]);

    let execution = live_adapter(blobs.clone())
        .generate(request)
        .await
        .expect("generate from image");

    assert!(
        assistant_text(&blobs, &execution)
            .await
            .to_lowercase()
            .contains("red")
    );
}

fn minimal_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref_offset = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        objects.len() + 1
    ));
    pdf.into_bytes()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_pdf_document() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let pdf_ref = blobs
        .put_bytes(minimal_pdf("The magic word is tangerine"))
        .await
        .expect("store PDF");
    let question_ref = text_blob(&blobs, "What is the magic word? Reply with one word.").await;
    let source = ContextEntrySource::RunInput {
        run_id: RunId::new(1),
        input_index: 0,
    };
    let mut pdf = entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source.clone(),
        pdf_ref,
    );
    pdf.content.media_type = Some("application/pdf".to_owned());
    pdf.preview = Some("[document: magic.pdf]".to_owned());

    let execution = live_adapter(blobs.clone())
        .generate(generation_request(vec![
            pdf,
            entry(
                2,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                source,
                question_ref,
            ),
        ]))
        .await
        .expect("generate from PDF");

    assert!(
        assistant_text(&blobs, &execution)
            .await
            .to_lowercase()
            .contains("tangerine")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_text_document() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let document_ref = text_blob(
        &blobs,
        "# Project facts\n\nThe release codename is **kingfisher**.",
    )
    .await;
    let question_ref = text_blob(
        &blobs,
        "What is the release codename? Reply with one lowercase word.",
    )
    .await;
    let source = ContextEntrySource::RunInput {
        run_id: RunId::new(1),
        input_index: 0,
    };
    let mut document = entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source.clone(),
        document_ref,
    );
    document.content.media_type = Some("text/markdown".to_owned());
    document.preview = Some("[document: facts.md]".to_owned());

    let execution = live_adapter(blobs.clone())
        .generate(generation_request(vec![
            document,
            entry(
                2,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                source,
                question_ref,
            ),
        ]))
        .await
        .expect("generate from text document");

    assert!(
        assistant_text(&blobs, &execution)
            .await
            .to_lowercase()
            .contains("kingfisher")
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_json_schema_output() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let prompt_ref = text_blob(
        &blobs,
        "Return Zurich and Switzerland using the requested JSON schema.",
    )
    .await;
    let mut request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        prompt_ref,
    )]);
    request.request.params = Some(openai_completions_params(&OpenAiCompletionsParams {
        response_format: Some(json!({
            "type":"json_schema",
            "json_schema": {
                "name":"location",
                "strict":true,
                "schema": {
                    "type":"object",
                    "properties": {
                        "city":{"type":"string"},
                        "country":{"type":"string"}
                    },
                    "required":["city","country"],
                    "additionalProperties":false
                }
            }
        })),
        store: Some(false),
        stream: Some(false),
        ..Default::default()
    }));

    let execution = live_adapter(blobs.clone())
        .generate(request)
        .await
        .expect("structured generation");
    let output = assistant_text(&blobs, &execution).await;
    let structured: serde_json::Value = serde_json::from_str(&output).expect("JSON output");
    assert_eq!(structured["city"], "Zurich");
    assert_eq!(structured["country"], "Switzerland");
}

async fn weather_tool(blobs: &InMemoryBlobStore) -> ToolSpec {
    let schema_ref = llm_runtime::blob_io::put_json(
        blobs,
        &json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
            "additionalProperties": false
        }),
    )
    .await
    .expect("schema");
    ToolSpec {
        name: ToolName::try_new("lookup_temperature").expect("tool name"),
        kind: ToolKind::Function(FunctionToolSpec {
            description_ref: Some(text_blob(blobs, "Look up a city's temperature").await),
            input_schema_ref: schema_ref,
            output_schema_ref: None,
            strict: Some(true),
            provider_options_ref: None,
        }),
        parallelism: ToolParallelism::ParallelSafe,
        execution: ToolExecutionSpec::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_tool_call_and_result_round_trip() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let prompt_ref = text_blob(
        &blobs,
        "Call lookup_temperature for Zurich and do not answer from memory.",
    )
    .await;
    let user_source = ContextEntrySource::RunInput {
        run_id: RunId::new(1),
        input_index: 0,
    };
    let mut first_request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        user_source.clone(),
        prompt_ref.clone(),
    )]);
    first_request.request.tools = vec![weather_tool(&blobs).await];
    first_request.request.tool_choice = Some(ToolChoice::Specific {
        tool_name: ToolName::try_new("lookup_temperature").expect("tool name"),
    });
    let adapter = live_adapter(blobs.clone());
    let first = adapter
        .generate(first_request)
        .await
        .expect("tool-call generation");
    assert_eq!(first.result.facts.finish, LlmFinish::ToolCalls);
    assert_eq!(first.result.facts.tool_calls.len(), 1);
    let observed = &first.result.facts.tool_calls[0];
    let args = blobs
        .read_text(&observed.arguments_ref)
        .await
        .expect("arguments")
        .to_lowercase();
    assert!(args.contains("zurich"));

    let assistant_source = ContextEntrySource::AssistantOutput {
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
    };
    let mut entries = vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        user_source,
        prompt_ref,
    )];
    for (index, output) in first.result.context_entries.iter().enumerate() {
        let mut committed = entry(
            index as u64 + 2,
            output.kind.clone(),
            assistant_source.clone(),
            output.content.content_ref.clone(),
        );
        committed.content.media_type = output.content.media_type.clone();
        committed.preview = output.preview.clone();
        committed.content.provider_kind = output.content.provider_kind.clone();
        committed.provenance_ref = output.provenance_ref.clone();
        entries.push(committed);
    }
    entries.push(entry(
        entries.len() as u64 + 1,
        ContextEntryKind::ToolResult {
            call_id: observed.call_id.clone(),
            is_error: false,
        },
        ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: None,
        },
        text_blob(&blobs, "{\"celsius\":21}").await,
    ));
    let mut second_request = generation_request(entries);
    second_request.turn_id = TurnId::new(2);
    second_request.request.tools = vec![weather_tool(&blobs).await];
    second_request.request.tool_choice = Some(ToolChoice::Auto);

    let second = adapter
        .generate(second_request)
        .await
        .expect("tool-result generation");

    assert!(
        assistant_text(&blobs, &second).await.contains("21"),
        "expected final answer to use the tool result"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_parallel_tool_calls() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let prompt_ref = text_blob(
        &blobs,
        "Call lookup_temperature once for Zurich and once for Tokyo. Make both calls now.",
    )
    .await;
    let mut request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        prompt_ref,
    )]);
    request.request.tools = vec![weather_tool(&blobs).await];
    request.request.tool_choice = Some(ToolChoice::RequiredAny);
    request.request.parallel_tool_use = Some(true);

    let execution = live_adapter(blobs.clone())
        .generate(request)
        .await
        .expect("parallel tool generation");

    assert_eq!(execution.result.facts.tool_calls.len(), 2);
    let mut arguments = String::new();
    for call in &execution.result.facts.tool_calls {
        arguments.push_str(
            &blobs
                .read_text(&call.arguments_ref)
                .await
                .expect("arguments")
                .to_lowercase(),
        );
    }
    assert!(arguments.contains("zurich"));
    assert!(arguments.contains("tokyo"));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_invalid_model_is_terminal_provider_error() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let prompt_ref = text_blob(&blobs, "hello").await;
    let mut request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::ContextEdit,
        prompt_ref,
    )]);
    request.request.model.model =
        "definitely-not-a-real-openai-model-for-lightspeed-tests".to_owned();

    let error = live_adapter(blobs)
        .generate(request)
        .await
        .expect_err("invalid model must fail");

    match error {
        LlmAdapterError::Provider { source } => assert!(!source.retryable()),
        other => panic!("expected provider error, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires DEEPSEEK_API_KEY (costs real money)"]
async fn deepseek_completions_runtime_live_v4_dialect_reasoning_and_usage() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let instructions_ref = text_blob(&blobs, "Follow exact-output requests.").await;
    let prompt_ref = text_blob(&blobs, "Compute 37 * 19. Reply with only the integer.").await;
    let request = deepseek_generation_request(vec![
        entry(
            1,
            ContextEntryKind::Instructions,
            ContextEntrySource::ContextEdit,
            instructions_ref,
        ),
        entry(
            2,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            prompt_ref,
        ),
    ]);

    let execution = deepseek_adapter(blobs.clone())
        .generate(request)
        .await
        .expect("DeepSeek generation");

    assert!(assistant_text(&blobs, &execution).await.contains("703"));
    let provider_request =
        llm_runtime::blob_io::read_json(blobs.as_ref(), &dumps(&execution).provider_request_ref)
            .await
            .expect("provider request");
    assert_eq!(provider_request["messages"][0]["role"], "system");
    assert_eq!(provider_request["max_tokens"], 512);
    assert!(provider_request.get("max_completion_tokens").is_none());
    assert_eq!(provider_request["thinking"], json!({"type":"enabled"}));

    let reasoning = execution
        .result
        .context_entries
        .iter()
        .find(|entry| {
            entry.content.provider_kind.as_deref()
                == Some(llm_runtime::openai_completions::OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND)
        })
        .expect("durable DeepSeek reasoning state");
    let reasoning = llm_runtime::blob_io::read_json(blobs.as_ref(), &reasoning.content.content_ref)
        .await
        .expect("reasoning state");
    assert!(
        reasoning["reasoning_content"]
            .as_str()
            .is_some_and(|reasoning| !reasoning.is_empty())
    );
    let usage = execution.result.facts.usage.expect("usage");
    assert!(usage.input_tokens.unwrap_or_default() > 0);
    assert!(usage.cached_input_tokens.is_some() || usage.cache_miss_input_tokens.is_some());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires DEEPSEEK_API_KEY (costs real money)"]
async fn deepseek_completions_runtime_live_v4_reasoning_tool_round_trip() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let prompt_ref = text_blob(
        &blobs,
        "Call lookup_temperature for Zurich, then report the returned temperature.",
    )
    .await;
    let user_source = ContextEntrySource::RunInput {
        run_id: RunId::new(1),
        input_index: 0,
    };
    let schema_ref = llm_runtime::blob_io::put_json(
        blobs.as_ref(),
        &json!({
            "type":"object",
            "properties":{"city":{"type":"string"}},
            "required":["city"]
        }),
    )
    .await
    .expect("schema");
    let tool = ToolSpec {
        name: ToolName::new("lookup_temperature"),
        kind: ToolKind::Function(FunctionToolSpec {
            description_ref: Some(text_blob(&blobs, "Look up a city's temperature").await),
            input_schema_ref: schema_ref,
            output_schema_ref: None,
            strict: None,
            provider_options_ref: None,
        }),
        parallelism: ToolParallelism::ParallelSafe,
        execution: ToolExecutionSpec::default(),
    };
    let mut first_request = deepseek_generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        user_source.clone(),
        prompt_ref.clone(),
    )]);
    first_request.request.tools = vec![tool.clone()];
    first_request.request.tool_choice = Some(ToolChoice::Auto);
    let adapter = deepseek_adapter(blobs.clone());
    let first = adapter
        .generate(first_request)
        .await
        .expect("DeepSeek tool-call generation");
    assert_eq!(first.result.facts.finish, LlmFinish::ToolCalls);
    assert_eq!(first.result.facts.tool_calls.len(), 1);
    assert!(first.result.context_entries.iter().any(|entry| {
        entry.content.provider_kind.as_deref()
            == Some(llm_runtime::openai_completions::OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND)
    }));
    let observed = first.result.facts.tool_calls[0].clone();

    let assistant_source = ContextEntrySource::AssistantOutput {
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
    };
    let mut entries = vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        user_source,
        prompt_ref,
    )];
    for (index, output) in first.result.context_entries.iter().enumerate() {
        let mut committed = entry(
            index as u64 + 2,
            output.kind.clone(),
            assistant_source.clone(),
            output.content.content_ref.clone(),
        );
        committed.content.media_type = output.content.media_type.clone();
        committed.preview = output.preview.clone();
        committed.content.provider_kind = output.content.provider_kind.clone();
        committed.provenance_ref = output.provenance_ref.clone();
        entries.push(committed);
    }
    entries.push(entry(
        entries.len() as u64 + 1,
        ContextEntryKind::ToolResult {
            call_id: observed.call_id,
            is_error: false,
        },
        ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: None,
        },
        text_blob(&blobs, "{\"celsius\":21}").await,
    ));
    let mut second_request = deepseek_generation_request(entries);
    second_request.turn_id = TurnId::new(2);
    second_request.request.tools = vec![tool];
    second_request.request.tool_choice = Some(ToolChoice::Auto);

    let second = adapter
        .generate(second_request)
        .await
        .expect("DeepSeek tool result generation with reasoning replay");
    assert!(
        assistant_text(&blobs, &second).await.contains("21"),
        "expected final DeepSeek answer to use the tool result"
    );
}

/// Live adapters run with debug dumps enabled so the tests can inspect the
/// exact provider exchange.
fn dumps(execution: &llm_runtime::LlmGenerationExecution) -> &llm_runtime::LlmDebugDumps {
    execution
        .debug_dumps
        .as_ref()
        .expect("live adapters are built with debug dumps enabled")
}

/// A tool result that hands the model two images and a PDF as media entries:
/// the model must tell the images apart by position, read the document, and
/// name the blue image by the handle the tool result announced.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_sees_tool_media() {
    use support::tool_media::{
        TOOL_MEDIA_PROMPT, assert_tool_media_answer, retained, tool_media_entries,
        view_images_tool_spec,
    };
    let blobs = Arc::new(InMemoryBlobStore::new());
    let prompt_ref = text_blob(&blobs, TOOL_MEDIA_PROMPT).await;
    let tool = view_images_tool_spec(&blobs).await;
    let user_source = ContextEntrySource::RunInput {
        run_id: RunId::new(1),
        input_index: 0,
    };
    let mut first_request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        user_source.clone(),
        prompt_ref.clone(),
    )]);
    first_request.request.tools = vec![tool.clone()];
    first_request.request.tool_choice = Some(ToolChoice::Specific {
        tool_name: ToolName::try_new("view_images").expect("tool name"),
    });
    first_request.request.output_limit = Some(2048);
    let adapter = live_adapter(blobs.clone());
    let first = adapter
        .generate(first_request)
        .await
        .expect("tool-call generation");
    assert_eq!(first.result.facts.finish, LlmFinish::ToolCalls);
    let observed = &first.result.facts.tool_calls[0];

    let mut entries = vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        user_source,
        prompt_ref,
    )];
    entries.extend(retained(2, &first.result.context_entries));
    let fixture =
        tool_media_entries(&blobs, observed.call_id.clone(), entries.len() as u64 + 1).await;
    entries.extend(fixture.entries.clone());
    let mut second_request = generation_request(entries);
    second_request.turn_id = TurnId::new(2);
    second_request.request.tools = vec![tool];
    second_request.request.tool_choice = Some(ToolChoice::Auto);
    second_request.request.output_limit = Some(2048);

    let second = adapter
        .generate(second_request)
        .await
        .expect("tool-result generation");
    assert_tool_media_answer(&assistant_text(&blobs, &second).await, &fixture);
}
