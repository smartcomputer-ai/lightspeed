use std::path::PathBuf;
use std::sync::Arc;

use engine::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND, ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
    BlobRef, ContextCompactionRequest, ContextCompactionStatus, ContextCompactionTask,
    ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ContextSnapshot, LlmFinish, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, LlmRequest, ModelSelection, ProviderApiKind, RunId, SessionId, ToolChoice,
    ToolName, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_clients::anthropic::messages::{Client, Config};
use llm_runtime::{AnthropicMessagesLlmAdapter, LlmCompactionAdapter, LlmGenerationAdapter};
use serde_json::{Value, json};

mod support;

use support::retrying_anthropic_messages_client;

fn live_model() -> String {
    env_or_dotenv_var("ANTHROPIC_MESSAGES_MODEL")
        .or_else(|_| env_or_dotenv_var("ANTHROPIC_LIVE_MODEL"))
        .unwrap_or_else(|_| "claude-opus-5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("ANTHROPIC_API_KEY").expect(
        "ANTHROPIC_API_KEY must be set in env or root .env to run llm-runtime anthropic:messages live tests",
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

async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
    blobs.insert_text(text).await
}

fn model_selection() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::AnthropicMessages,
        provider_id: "anthropic".to_string(),
        model: live_model(),
    }
}

fn user_entry(entry_id: u64, content_ref: BlobRef) -> ContextEntry {
    ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(entry_id),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
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
        supersedes: None,
    }
}

fn intent_request(fingerprint: &str, entries: Vec<ContextEntry>) -> LlmRequest {
    LlmRequest {
        model: model_selection(),
        request_fingerprint: fingerprint.to_string(),
        context: ContextSnapshot {
            api_kind: ProviderApiKind::AnthropicMessages,
            context_revision: 0,
            entries,
            token_estimate: None,
        },
        tools: Vec::new(),
        tool_choice: None,
        // Thinking counts toward the cap on models that reason by default.
        output_limit: Some(4096),
        reasoning_effort: None,
        parallel_tool_use: None,
        processing_tier: None,
        provider_response_id: None,
        compaction: None,
        params: None,
    }
}

/// The reasoning entries must carry the provider's summary text (not the
/// opaque marker an omitted display leaves behind), and usage must report the
/// billed thinking tokens.
fn assert_visible_thinking(result: &LlmGenerationResult, label: &str) {
    let previews = result
        .context_entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::ReasoningState))
        .map(|entry| entry.preview.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(
        !previews.is_empty(),
        "{label}: expected a reasoning state entry from thinking blocks, got {:?}",
        result.context_entries
    );
    assert!(
        previews.iter().any(|preview| {
            let lower = preview.trim().to_lowercase();
            !lower.is_empty() && lower != "reasoning state" && lower != "redacted thinking"
        }),
        "{label}: expected summarized thinking text in the reasoning entries, got {previews:?}"
    );
    let reasoning_tokens = result
        .facts
        .usage
        .as_ref()
        .and_then(|usage| usage.reasoning_tokens)
        .unwrap_or_default();
    assert!(
        reasoning_tokens > 0,
        "{label}: expected billed thinking tokens in usage, got {:?}",
        result.facts.usage
    );
}

async fn provider_request_json(blobs: &InMemoryBlobStore, execution_ref: &BlobRef) -> Value {
    let raw = blobs
        .read_text(execution_ref)
        .await
        .expect("provider request blob");
    serde_json::from_str(&raw).expect("provider request json")
}

fn generation_request(turn_id: u64, request: LlmRequest) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("session-live-anthropic"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(turn_id),
        request,
    }
}

fn retained_context_entry(index: usize, item: &ContextEntryInput) -> ContextEntry {
    ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(index as u64 + 1),
        kind: item.kind.clone(),
        source: match item.kind {
            ContextEntryKind::ReasoningState => ContextEntrySource::Reasoning {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
            },
            _ => ContextEntrySource::AssistantOutput {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
            },
        },
        content: item.content.clone(),
        preview: item.preview.clone(),
        origin: None,
        provenance_ref: item.provenance_ref.clone(),
        token_estimate: item.token_estimate.clone(),
        supersedes: None,
    }
}

fn weather_tool_spec(schema_ref: BlobRef, description_ref: BlobRef) -> engine::ToolSpec {
    engine::ToolSpec {
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
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_generates_result() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(
        &blobs,
        "Reply with exactly these two words: lightspeed adapter",
    )
    .await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = generation_request(
        1,
        intent_request("live-anthropic-messages", vec![user_entry(1, input_ref)]),
    );

    let execution = adapter.generate(request).await.expect("generate message");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(execution.result.facts.finish, LlmFinish::Stop);
    assert!(
        execution
            .result
            .facts
            .provider_response_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()),
        "expected provider response id"
    );
    assert!(
        execution
            .result
            .facts
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens)
            .unwrap_or_default()
            > 0,
        "expected usage tokens"
    );
    let assistant_ref = execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("assistant context item");
    let assistant_text = support::content_text(blobs.as_ref(), &assistant_ref).await;
    assert!(
        assistant_text.to_lowercase().contains("lightspeed"),
        "expected assistant output to contain lightspeed, got {assistant_text:?}"
    );

    let provider_request = blobs
        .read_text(&dumps(&execution).provider_request_ref)
        .await
        .expect("provider request blob");
    assert!(
        provider_request.contains("\"model\""),
        "expected provider request JSON, got {provider_request}"
    );
    let raw_response = blobs
        .read_text(&dumps(&execution).raw_response_ref)
        .await
        .expect("raw response blob");
    assert!(
        raw_response.contains("\"id\""),
        "expected raw response JSON, got {raw_response}"
    );
}

fn assert_hosted_web_result(result: &LlmGenerationResult, capability: &str) {
    assert_eq!(result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(result.facts.finish, LlmFinish::Stop);
    assert!(
        result.context_entries.iter().any(|entry| {
            entry.content.provider_kind.as_deref()
                == Some(ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND)
        }),
        "expected {capability} server_tool_use block: {:?}",
        result.context_entries
    );
    assert!(
        result.context_entries.iter().any(|entry| {
            entry.content.provider_kind.as_deref()
                == Some(ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND)
        }),
        "expected {capability} server-tool result block: {:?}",
        result.context_entries
    );
    assert!(
        cited_text_blocks(result).is_some(),
        "expected {capability} native assistant text blocks: {:?}",
        result.context_entries
    );
}

/// The final native assistant message owns its text and citations.
fn cited_text_blocks(result: &LlmGenerationResult) -> Option<&ContextEntryInput> {
    result.context_entries.iter().rev().find(|entry| {
        matches!(
            entry.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ) && entry.content.provider_kind.as_deref()
            == Some(ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND)
    })
}

async fn assert_hosted_web_replay(
    blobs: &Arc<InMemoryBlobStore>,
    adapter: &AnthropicMessagesLlmAdapter,
    original: &LlmRequest,
    result: &LlmGenerationResult,
) {
    let mut request = original.clone();
    let offset = request.context.entries.len();
    request.context.entries.extend(
        result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| retained_context_entry(offset + index, entry)),
    );
    let projected = api_projection::CoreAgentProjector::new(blobs.as_ref())
        .project_context_state(0, &request.context.entries)
        .await
        .expect("project sourced history");
    assert!(
        projected
            .entries
            .iter()
            .any(|entry| !entry.citations.is_empty())
    );
    let text = "Using only the sources already above, summarize the answer in one sentence. Do not call any tools.";
    let prompt = text_blob(blobs, text).await;
    request
        .context
        .entries
        .push(user_entry(request.context.entries.len() as u64 + 1, prompt));
    request.tool_choice = Some(ToolChoice::Auto);
    request.provider_response_id = None;
    request.output_limit = Some(512);
    let followup = adapter
        .generate(generation_request(2, request))
        .await
        .expect("replay native web history");
    assert_eq!(followup.result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(followup.result.facts.finish, LlmFinish::Stop);
    let message = cited_text_blocks(&followup.result).expect("native follow-up message");
    assert!(
        !support::content_text(blobs.as_ref(), &message.content)
            .await
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY and a model supporting hosted web search (costs real money)"]
async fn anthropic_messages_live_adapter_uses_hosted_web_search() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let tool = tools::definitions::register(
        "web.search",
        Default::default(),
        engine::ToolParallelism::ParallelSafe,
        Default::default(),
    );
    let input_ref = text_blob(
        &blobs,
        "Use web search to find Anthropic's current web search tool documentation. Reply with one short sourced sentence.",
    )
    .await;
    let mut request = intent_request(
        "live-anthropic-messages-web-search",
        vec![user_entry(1, input_ref)],
    );
    request.tools = vec![tool];
    request.tool_choice = Some(ToolChoice::Specific {
        tool_name: ToolName::new("web.search"),
    });
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let execution = adapter
        .generate(generation_request(1, request.clone()))
        .await
        .expect("generate with hosted web search");

    assert_hosted_web_result(&execution.result, "web search");
    let cited = cited_text_blocks(&execution.result).expect("cited blocks");
    let native_blocks = blobs
        .read_text(&cited.content.content_ref)
        .await
        .expect("native blocks");
    let native_blocks: Value = serde_json::from_str(&native_blocks).expect("native blocks JSON");
    assert!(
        native_blocks
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|block| {
                block
                    .get("citations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .any(|citation| {
                citation.get("type").and_then(Value::as_str) == Some("web_search_result_location")
            }),
        "expected native search citations in the retained blocks: {native_blocks}"
    );
    let provider_request =
        provider_request_json(&blobs, &dumps(&execution).provider_request_ref).await;
    assert_eq!(provider_request["tools"][0]["type"], "web_search_20250305");
    assert_hosted_web_replay(&blobs, &adapter, &request, &execution.result).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY and a model supporting hosted web fetch (costs real money)"]
async fn anthropic_messages_live_adapter_uses_hosted_web_fetch() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let tool = tools::definitions::register(
        "web.fetch",
        Default::default(),
        engine::ToolParallelism::ParallelSafe,
        Default::default(),
    );
    let input_ref = text_blob(
        &blobs,
        "Use web_fetch to read https://www.rfc-editor.org/rfc/rfc2606.txt. State which top-level domains it reserves and cite the fetched document.",
    )
    .await;
    let mut request = intent_request(
        "live-anthropic-messages-web-fetch",
        vec![user_entry(1, input_ref)],
    );
    request.tools = vec![tool];
    request.tool_choice = Some(ToolChoice::Specific {
        tool_name: ToolName::new("web.fetch"),
    });
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let execution = adapter
        .generate(generation_request(1, request.clone()))
        .await
        .expect("generate with hosted web fetch");

    assert_hosted_web_result(&execution.result, "web fetch");
    let cited = cited_text_blocks(&execution.result).expect("cited blocks");
    let native_blocks = blobs
        .read_text(&cited.content.content_ref)
        .await
        .expect("native blocks");
    let native_blocks: Value = serde_json::from_str(&native_blocks).expect("native blocks JSON");
    assert!(
        native_blocks
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|block| {
                block
                    .get("citations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .any(|citation| citation.get("document_index").is_some()),
        "expected document-located fetch citations in the retained blocks: {native_blocks}"
    );
    let provider_request =
        provider_request_json(&blobs, &dumps(&execution).provider_request_ref).await;
    assert_eq!(provider_request["tools"][0]["type"], "web_fetch_20250910");
    assert_eq!(provider_request["tools"][0]["citations"]["enabled"], true);
    assert_hosted_web_replay(&blobs, &adapter, &request, &execution.result).await;
}

/// 32x32 solid red PNG.
const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_describes_image_input() {
    use base64::Engine as _;
    let blobs = Arc::new(InMemoryBlobStore::new());
    let image_bytes = base64::engine::general_purpose::STANDARD
        .decode(RED_PNG_BASE64)
        .expect("decode test png");
    let image_ref = blobs.put_bytes(image_bytes).await.expect("store image");
    let question_ref = text_blob(
        &blobs,
        "What is the dominant color of this image? Reply with one English word in lowercase.",
    )
    .await;

    let mut image_entry = user_entry(1, image_ref);
    image_entry.content.media_type = Some("image/png".to_owned());
    image_entry.preview = Some("[image: red.png]".to_owned());
    let question_entry = user_entry(2, question_ref);

    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = generation_request(
        1,
        intent_request(
            "live-anthropic-messages-image",
            vec![image_entry, question_entry],
        ),
    );

    let execution = adapter.generate(request).await.expect("generate message");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    let assistant_ref = execution
        .result
        .context_entries
        .iter()
        .find(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                }
            )
        })
        .map(|entry| entry.content.clone())
        .expect("assistant entry");
    let answer = support::content_text(blobs.as_ref(), &assistant_ref)
        .await
        .to_lowercase();
    assert!(
        answer.contains("red"),
        "expected the model to identify the red image, got: {answer}"
    );
}

/// A minimal one-page PDF with correct xref offsets carrying `text`.
fn minimal_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref_offset = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
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
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_reads_pdf_document_input() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let pdf_ref = blobs
        .put_bytes(minimal_pdf("The magic word is tangerine"))
        .await
        .expect("store pdf");
    let question_ref = text_blob(
        &blobs,
        "What is the magic word in the attached document? Reply with one English word in lowercase.",
    )
    .await;

    let mut pdf_entry = user_entry(1, pdf_ref);
    pdf_entry.content.media_type = Some("application/pdf".to_owned());
    pdf_entry.preview = Some("[document: magic.pdf]".to_owned());
    let question_entry = user_entry(2, question_ref);

    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = generation_request(
        1,
        intent_request(
            "live-anthropic-messages-pdf",
            vec![pdf_entry, question_entry],
        ),
    );

    let execution = adapter.generate(request).await.expect("generate message");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    let assistant_ref = execution
        .result
        .context_entries
        .iter()
        .find(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                }
            )
        })
        .map(|entry| entry.content.clone())
        .expect("assistant entry");
    let answer = support::content_text(blobs.as_ref(), &assistant_ref)
        .await
        .to_lowercase();
    assert!(
        answer.contains("tangerine"),
        "expected the model to read the PDF magic word, got: {answer}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_runs_tool_round_trip() {
    let blobs = Arc::new(InMemoryBlobStore::new());
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
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let mut request = intent_request(
        "live-anthropic-messages-tool",
        vec![user_entry(1, input_ref.clone())],
    );
    request.tools = vec![weather_tool_spec(
        schema_ref.clone(),
        description_ref.clone(),
    )];
    request.tool_choice = Some(ToolChoice::RequiredAny);
    request.parallel_tool_use = Some(false);

    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("generate tool call");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(execution.result.facts.finish, LlmFinish::ToolCalls);
    let tool_call = execution
        .result
        .facts
        .tool_calls
        .first()
        .expect("observed tool call");
    assert_eq!(tool_call.tool_name, ToolName::new("get_weather"));
    let arguments = blobs
        .read_text(&tool_call.arguments_ref)
        .await
        .expect("tool arguments");
    assert!(
        arguments.to_lowercase().contains("zurich"),
        "expected tool arguments to mention Zurich, got {arguments:?}"
    );

    // Feed the tool result back and ask for the final answer, replaying the
    // assistant tool_use entry exactly as retained.
    let mut entries = vec![user_entry(1, input_ref)];
    let offset = entries.len();
    entries.extend(
        execution
            .result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(offset + index, item)),
    );
    let tool_output_ref = text_blob(&blobs, "11°C and sunny").await;
    entries.push(ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(entries.len() as u64 + 1),
        kind: ContextEntryKind::ToolResult {
            call_id: tool_call.call_id.clone(),
            is_error: false,
        },
        source: ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: None,
        },
        content: engine::ContentRef::text(tool_output_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    });

    let mut followup = intent_request("live-anthropic-messages-tool-followup", entries);
    followup.tools = vec![weather_tool_spec(schema_ref, description_ref)];

    let followup_execution = adapter
        .generate(generation_request(2, followup))
        .await
        .expect("generate final answer");

    assert_eq!(
        followup_execution.result.status,
        LlmGenerationStatus::Succeeded
    );
    let final_ref = followup_execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("final assistant context item");
    let final_text = support::content_text(blobs.as_ref(), &final_ref).await;
    assert!(
        final_text.contains("11"),
        "expected final answer to use the tool result, got {final_text:?}"
    );
}

/// The product path: the session's `reasoningEffort` alone must yield
/// visible (summarized) thinking, billed thinking tokens in usage, and
/// signed blocks that replay on the next turn.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_preserves_thinking_blocks() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(
        &blobs,
        "Compute 13 * 17 and 29 * 31, then their sum. Think it through carefully, \
         then reply with just the final number.",
    )
    .await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let mut request = intent_request(
        "live-anthropic-messages-thinking",
        vec![user_entry(1, input_ref.clone())],
    );
    request.output_limit = Some(8192);
    request.reasoning_effort = Some("high".to_string());

    let execution = adapter
        .generate(generation_request(1, request.clone()))
        .await
        .expect("generate with thinking");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    let provider_request =
        provider_request_json(&blobs, &dumps(&execution).provider_request_ref).await;
    assert_eq!(
        provider_request["thinking"],
        json!({ "type": "adaptive", "display": "summarized" }),
        "the effort tier must derive adaptive thinking with a visible summary"
    );
    assert_eq!(
        provider_request["output_config"],
        json!({ "effort": "high" })
    );
    assert_visible_thinking(&execution.result, "first turn");
    let answer_ref = execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("assistant answer");
    let answer = support::content_text(blobs.as_ref(), &answer_ref).await;
    assert!(answer.contains("1120"), "expected 1120, got {answer:?}");

    // Replay the retained thinking + answer entries with a follow-up question
    // to prove signed thinking blocks survive the round trip.
    let mut entries = vec![user_entry(1, input_ref)];
    let offset = entries.len();
    entries.extend(
        execution
            .result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(offset + index, item)),
    );
    let followup_ref = text_blob(&blobs, "Now add 4 to that. Reply with just the number.").await;
    entries.push(user_entry(entries.len() as u64 + 1, followup_ref));
    let mut followup = intent_request("live-anthropic-messages-thinking-followup", entries);
    followup.output_limit = request.output_limit;
    followup.reasoning_effort = request.reasoning_effort;

    let followup_execution = adapter
        .generate(generation_request(2, followup))
        .await
        .expect("generate follow-up after thinking replay");

    assert_eq!(
        followup_execution.result.status,
        LlmGenerationStatus::Succeeded
    );
    let followup_answer_ref = followup_execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("follow-up answer");
    let followup_answer = support::content_text(blobs.as_ref(), &followup_answer_ref).await;
    assert!(
        followup_answer.contains("1124"),
        "expected 1124, got {followup_answer:?}"
    );
}

/// Interleaved thinking through a tool loop on the product path. Both turns
/// need reasoning (which city; Kelvin to Celsius) — adaptive thinking skips
/// trivial dispatch even at `xhigh`, the tier admission used to reject — so
/// the model thinks before the call and again after the result; every
/// summarized, signed block must replay unchanged next to its `tool_use`,
/// and the tool call follows the thinking that explains it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_thinks_across_tool_round_trip() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(
        &blobs,
        "I am in the capital of Switzerland. Work out which city that is, look up its current \
         temperature with the get_weather tool, and finally answer with the temperature in \
         degrees Celsius. Think carefully before each step.",
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
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let mut request = intent_request(
        "live-anthropic-messages-thinking-tool",
        vec![user_entry(1, input_ref.clone())],
    );
    request.output_limit = Some(8192);
    request.reasoning_effort = Some("xhigh".to_string());
    request.tools = vec![weather_tool_spec(
        schema_ref.clone(),
        description_ref.clone(),
    )];
    request.parallel_tool_use = Some(false);

    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("generate tool call with thinking");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(execution.result.facts.finish, LlmFinish::ToolCalls);
    let provider_request =
        provider_request_json(&blobs, &dumps(&execution).provider_request_ref).await;
    assert_eq!(
        provider_request["output_config"],
        json!({ "effort": "xhigh" })
    );
    assert_eq!(
        provider_request["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_visible_thinking(&execution.result, "tool call turn");
    let kinds = execution
        .result
        .context_entries
        .iter()
        .map(|entry| entry.kind.clone())
        .collect::<Vec<_>>();
    let first_thinking = kinds
        .iter()
        .position(|kind| matches!(kind, ContextEntryKind::ReasoningState))
        .expect("reasoning entry");
    let first_tool_call = kinds
        .iter()
        .position(|kind| matches!(kind, ContextEntryKind::ToolCall { .. }))
        .expect("tool call entry");
    assert!(
        first_thinking < first_tool_call,
        "thinking must precede the tool call it explains, got {kinds:?}"
    );
    let tool_call = execution
        .result
        .facts
        .tool_calls
        .first()
        .expect("observed tool call");
    assert_eq!(tool_call.tool_name, ToolName::new("get_weather"));
    let arguments = blobs
        .read_text(&tool_call.arguments_ref)
        .await
        .expect("tool arguments");
    assert!(
        arguments.to_lowercase().contains("bern"),
        "expected the model to reason its way to Bern, got {arguments:?}"
    );

    // Replay thinking + tool_use exactly as retained, then a tool result the
    // model has to convert before answering.
    let mut entries = vec![user_entry(1, input_ref)];
    let offset = entries.len();
    entries.extend(
        execution
            .result
            .context_entries
            .iter()
            .enumerate()
            .map(|(index, item)| retained_context_entry(offset + index, item)),
    );
    let tool_output_ref = text_blob(&blobs, "284.15 K and sunny").await;
    entries.push(ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(entries.len() as u64 + 1),
        kind: ContextEntryKind::ToolResult {
            call_id: tool_call.call_id.clone(),
            is_error: false,
        },
        source: ContextEntrySource::Tool {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: None,
        },
        content: engine::ContentRef::text(tool_output_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    });

    let mut followup = intent_request("live-anthropic-messages-thinking-tool-followup", entries);
    followup.output_limit = Some(8192);
    followup.reasoning_effort = Some("xhigh".to_string());
    followup.tools = vec![weather_tool_spec(schema_ref, description_ref)];

    let followup_execution = adapter
        .generate(generation_request(2, followup))
        .await
        .expect("generate final answer after replaying thinking + tool_use");

    assert_eq!(
        followup_execution.result.status,
        LlmGenerationStatus::Succeeded
    );
    // Interleaved thinking: the model reasons about the tool result too.
    assert_visible_thinking(&followup_execution.result, "follow-up turn");
    let final_ref = followup_execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("final assistant context item");
    let final_text = support::content_text(blobs.as_ref(), &final_ref).await;
    assert!(
        final_text.contains("11"),
        "expected final answer to use the tool result, got {final_text:?}"
    );
}

/// With no `maxOutputTokens` on the session the adapter sends its 32K default
/// (Anthropic requires the field; the OpenAI adapters send none) and the
/// provider accepts it on a plain non-streaming request.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_default_output_cap_is_accepted() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, "Reply with exactly: ok").await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let mut request = intent_request(
        "live-anthropic-messages-default-cap",
        vec![user_entry(1, input_ref)],
    );
    request.output_limit = None;

    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("generate with the default cap");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(execution.result.facts.finish, LlmFinish::Stop);
    let provider_request =
        provider_request_json(&blobs, &dumps(&execution).provider_request_ref).await;
    assert_eq!(
        provider_request["max_tokens"],
        json!(32_768),
        "the adapter default must be sent when the session sets no cap"
    );
    let answer_ref = execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("assistant answer");
    let answer = support::content_text(blobs.as_ref(), &answer_ref).await;
    assert!(answer.to_lowercase().contains("ok"), "got {answer:?}");
}

/// A turn cut off at `max_tokens` fails the run but keeps the partial text.
/// Thinking is off (`reasoningEffort: none` → `thinking: disabled`, proved on
/// the wire here) so the tiny cap lands on visible output rather than on
/// reasoning.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_fails_the_turn_on_truncation_but_keeps_partial_text() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(
        &blobs,
        "Write a 400-word essay about the history of the bicycle. Begin immediately with the \
         essay text.",
    )
    .await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let mut request = intent_request(
        "live-anthropic-messages-truncation",
        vec![user_entry(1, input_ref)],
    );
    request.output_limit = Some(48);
    request.reasoning_effort = Some("none".to_string());

    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("a truncated response is a response, not a transport error");

    let provider_request =
        provider_request_json(&blobs, &dumps(&execution).provider_request_ref).await;
    assert_eq!(provider_request["max_tokens"], json!(48));
    assert_eq!(provider_request["thinking"], json!({ "type": "disabled" }));
    assert_eq!(
        execution.result.status,
        LlmGenerationStatus::Failed,
        "expected the cut-off to fail the turn, got {:?}",
        execution.result
    );
    assert_eq!(execution.result.facts.finish, LlmFinish::Length);
    assert!(execution.result.facts.tool_calls.is_empty());
    let partial = execution
        .result
        .context_entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant
                }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        partial.len(),
        execution.result.context_entries.len(),
        "only the partial text may be kept, got {:?}",
        execution.result.context_entries
    );
    assert_eq!(partial.len(), 1, "expected the partial essay text");
    let text = blobs
        .read_text(&partial[0].content.content_ref)
        .await
        .expect("partial text");
    assert!(!text.trim().is_empty(), "partial text must be visible");
    let failure_ref = execution
        .result
        .failure_ref
        .clone()
        .expect("truncation failure ref");
    let failure = blobs.read_text(&failure_ref).await.expect("failure text");
    assert!(
        failure.contains("cut off at max output tokens 48")
            && failure.contains("partial output is kept"),
        "expected the cap in the failure text, got {failure:?}"
    );
}

/// A safety-classifier refusal must fail the turn with its category, never
/// complete it as an empty answer, and nothing may fall back to another
/// model. Claude Opus 5's real-time cyber safeguard has refused exactly this
/// note plus summary request every time it was sent (`stop_reason: refusal`,
/// category `cyber`, zero output); if the classifier ever stops refusing it,
/// this test fails on the status assertion and the fixture needs a new
/// trigger — the adapter path it proves is unchanged.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_fails_the_turn_on_refusal() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let notes_ref = text_blob(
        &blobs,
        "Project kickoff notes: we are wiring the deployment pipeline this week. The release \
         codename for this rollout is LIGHTSPEED-ANTHROPIC-COMPACTION-LIVE-4217; the ops team \
         uses it to tag every artifact. We also decided to store session logs in Postgres.",
    )
    .await;
    let ask_ref = text_blob(
        &blobs,
        "Summarize the conversation above for context compaction. Capture the user's goals, \
         decisions made, work completed, important tool results, and open questions. The \
         summary will replace the prior conversation history, so include everything needed to \
         continue seamlessly. Reply with the summary only. Keep the summary under 256 tokens.",
    )
    .await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = intent_request(
        "live-anthropic-messages-refusal",
        vec![user_entry(1, notes_ref), user_entry(2, ask_ref)],
    );

    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("a refusal is a response, not a transport error");

    assert_eq!(
        execution.result.status,
        LlmGenerationStatus::Failed,
        "expected the classifier refusal to fail the turn, got {:?}",
        execution.result
    );
    assert_eq!(execution.result.facts.finish, LlmFinish::ContentFilter);
    assert!(
        execution.result.context_entries.is_empty(),
        "a refused turn must not land content in the session log"
    );
    let failure_ref = execution
        .result
        .failure_ref
        .clone()
        .expect("refusal failure ref");
    let failure = blobs.read_text(&failure_ref).await.expect("failure text");
    assert!(
        failure.contains("Anthropic refused response") && failure.contains("(category: "),
        "expected the refusal category in the failure text, got {failure:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_summarizes_context_compaction() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let first_ref = text_blob(
        &blobs,
        "Remember this codename: ZEPHYR-42. We will need it later in the project.",
    )
    .await;
    let second_ref = text_blob(
        &blobs,
        "We decided to store session logs in Postgres and blobs in a content-addressed store.",
    )
    .await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = ContextCompactionRequest {
        session_id: SessionId::new("session-live-anthropic-compaction"),
        request: ContextCompactionTask {
            model: model_selection(),
            request_fingerprint: "live-anthropic-messages-compaction".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::AnthropicMessages,
                context_revision: 7,
                entries: vec![user_entry(1, first_ref), user_entry(2, second_ref)],
                token_estimate: None,
            },
            target_tokens: Some(300),
            params: None,
        },
    };

    let result = adapter
        .compact_context(request)
        .await
        .expect("compact context");

    assert_eq!(result.status, ContextCompactionStatus::Succeeded);
    assert_eq!(result.context_revision, 7);
    assert_eq!(result.context_entries.len(), 1);
    let entry = &result.context_entries[0];
    assert!(matches!(
        entry.kind,
        ContextEntryKind::Message {
            role: ContextMessageRole::User
        }
    ));
    assert_eq!(
        entry.content.provider_kind.as_deref(),
        Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
    );
    let summary = blobs
        .read_text(&entry.content.content_ref)
        .await
        .expect("summary text");
    assert!(
        summary.to_uppercase().contains("ZEPHYR"),
        "expected the summary to retain the codename, got {summary:?}"
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

/// A tool result that hands the model an image as a media entry: the model
/// must see it and name it by the handle the tool result announced.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_sees_tool_image() {
    tool_media_round_trip(live_model(), support::tool_media::ToolMediaSet::SingleImage).await;
}

/// Two images and a PDF from one tool result. Claude Opus 5's refusal
/// classifier (`reasoning_extraction`) rejects some tool-media follow-ups,
/// reliably so when a PDF arrives after a tool round-trip, while the other
/// Claude models read them, so this runs on Sonnet 5 by default;
/// `ANTHROPIC_TOOL_MEDIA_MODEL` overrides.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_sees_tool_media() {
    let model = env_or_dotenv_var("ANTHROPIC_TOOL_MEDIA_MODEL")
        .unwrap_or_else(|_| "claude-sonnet-5".to_owned());
    tool_media_round_trip(model, support::tool_media::ToolMediaSet::ImagesAndPdf).await;
}

async fn tool_media_round_trip(model: String, set: support::tool_media::ToolMediaSet) {
    use support::tool_media::{
        assert_tool_media_answer, assistant_entry, retained, tool_media_entries_for,
        view_images_tool_spec,
    };
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, set.prompt()).await;
    let tool = view_images_tool_spec(&blobs).await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let mut request = intent_request(
        "live-anthropic-messages-tool-media",
        vec![user_entry(1, input_ref.clone())],
    );
    request.model.model = model.clone();
    request.tools = vec![tool.clone()];
    request.tool_choice = Some(ToolChoice::RequiredAny);
    request.parallel_tool_use = Some(false);
    let execution = adapter
        .generate(generation_request(1, request))
        .await
        .expect("generate tool call");
    assert_eq!(execution.result.facts.finish, LlmFinish::ToolCalls);
    let tool_call = execution
        .result
        .facts
        .tool_calls
        .first()
        .expect("observed tool call");
    assert_eq!(tool_call.tool_name, ToolName::new("view_images"));

    let mut entries = vec![user_entry(1, input_ref)];
    entries.extend(retained(2, &execution.result.context_entries));
    let fixture = tool_media_entries_for(
        &blobs,
        tool_call.call_id.clone(),
        entries.len() as u64 + 1,
        set,
    )
    .await;
    entries.extend(fixture.entries.clone());

    let mut followup = intent_request("live-anthropic-messages-tool-media-followup", entries);
    followup.model.model = model;
    followup.tools = vec![tool];
    let followup_execution = adapter
        .generate(generation_request(2, followup))
        .await
        .expect("generate final answer");
    if followup_execution.result.status != LlmGenerationStatus::Succeeded {
        let failure = match &followup_execution.result.failure_ref {
            Some(failure_ref) => blobs.read_text(failure_ref).await.unwrap_or_default(),
            None => String::new(),
        };
        panic!("follow-up generation failed: {failure}");
    }
    let final_text =
        support::content_text(blobs.as_ref(), &assistant_entry(&followup_execution)).await;
    assert_tool_media_answer(&final_text, &fixture);
}
