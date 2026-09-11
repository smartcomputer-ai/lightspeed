use std::path::PathBuf;
use std::sync::Arc;

use engine::{
    BlobRef, CompactionPolicy, ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ContextSnapshot, LlmFinish, LlmGenerationRequest, LlmGenerationStatus,
    LlmRequest, ModelSelection, OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND,
    OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND, OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND,
    ProviderApiKind, ProviderParams, RunId, SessionId, ToolChoice, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_clients::openai::responses::{Client, Config};
use llm_runtime::{
    LlmGenerationAdapter, OpenAiResponsesLlmAdapter, OpenAiResponsesParams,
    params::{
        OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE,
        OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE,
    },
};
use serde_json::{Value, json};
use tools::web::search::{OpenAiResponsesWebSearchConfig, WebSearchContextSize, WebSearchMode};

mod support;

use support::retrying_openai_responses_client;

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_web_search_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_WEB_SEARCH_MODEL").unwrap_or_else(|_| live_model())
}

fn live_compaction_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_COMPACTION_MODEL").unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("OPENAI_API_KEY").expect(
        "OPENAI_API_KEY must be set in env or root .env to run llm-runtime openai:responses live tests",
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

async fn text_blob(blobs: &InMemoryBlobStore, text: &str) -> BlobRef {
    blobs.insert_text(text).await
}

fn openai_params(params: &OpenAiResponsesParams) -> ProviderParams {
    ProviderParams::new(
        ProviderApiKind::OpenAiResponses,
        serde_json::to_value(params).expect("serialize params"),
    )
}

/// 32x32 solid red PNG.
const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and Fast mode access (costs real money)"]
async fn openai_responses_live_fast_mode_reports_effective_service_tier() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, "Reply with exactly: fast mode ok").await;
    let model =
        env_or_dotenv_var("OPENAI_FAST_MODE_MODEL").unwrap_or_else(|_| "gpt-5.6-sol".to_owned());
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live-fast-mode"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model,
            },
            request_fingerprint: "live-openai-responses-fast-mode".to_owned(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries: vec![ContextEntry {
                    key: None,
                    entry_id: ContextEntryId::new(1),
                    kind: ContextEntryKind::Message {
                        role: ContextMessageRole::User,
                    },
                    source: ContextEntrySource::RunInput {
                        run_id: RunId::new(1),
                        input_index: 0,
                    },
                    content: engine::ContentRef {
                        content_ref: input_ref,
                        media_type: None,
                        provider_kind: None,
                    },
                    preview: None,
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                    supersedes: None,
                }],
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(64),
            reasoning_effort: Some("none".to_owned()),
            parallel_tool_use: None,
            processing_tier: Some(engine::ModelProcessingTier::Fast),
            provider_response_id: None,
            compaction: None,
            params: Some(openai_params(&OpenAiResponsesParams {
                store: Some(false),
                stream: Some(false),
                ..OpenAiResponsesParams::default()
            })),
        },
    };

    let execution = adapter.generate(request).await.expect("Fast mode response");
    let provider_request: Value = serde_json::from_str(
        &blobs
            .read_text(&dumps(&execution).provider_request_ref)
            .await
            .expect("provider request"),
    )
    .expect("provider request JSON");
    assert_eq!(provider_request["service_tier"], "fast");

    let raw_response: Value = serde_json::from_str(
        &blobs
            .read_text(&dumps(&execution).raw_response_ref)
            .await
            .expect("raw response"),
    )
    .expect("raw response JSON");
    assert!(
        matches!(
            raw_response.get("service_tier").and_then(Value::as_str),
            Some("fast" | "priority")
        ),
        "expected Fast mode response tier, got {raw_response}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_adapter_describes_image_input() {
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

    let entries = vec![
        ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef {
                content_ref: image_ref,
                media_type: Some("image/png".to_owned()),
                provider_kind: None,
            },
            preview: Some("[image: red.png]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        },
        ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(2),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 1,
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
            supersedes: None,
        },
    ];
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live-image"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_string(),
                model: live_model(),
            },
            request_fingerprint: "live-openai-responses-image".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries,
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(512),
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
        },
    };

    let execution = adapter.generate(request).await.expect("generate response");

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
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_adapter_reads_pdf_document_input() {
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

    let entries = vec![
        ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(1),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
            content: engine::ContentRef {
                content_ref: pdf_ref,
                media_type: Some("application/pdf".to_owned()),
                provider_kind: None,
            },
            preview: Some("[document: magic.pdf]".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        },
        ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(2),
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source: ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 1,
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
            supersedes: None,
        },
    ];
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live-pdf"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_string(),
                model: live_model(),
            },
            request_fingerprint: "live-openai-responses-pdf".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries,
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(512),
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
        },
    };

    let execution = adapter.generate(request).await.expect("generate response");

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
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_adapter_generates_result() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(
        &blobs,
        "Reply with exactly these two words: lightspeed adapter",
    )
    .await;
    let context_entry = ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(1),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        content: engine::ContentRef {
            content_ref: input_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    };
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_string(),
                model: live_model(),
            },
            request_fingerprint: "live-openai-responses".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries: vec![context_entry],
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(512),
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
        },
    };

    let execution = adapter.generate(request).await.expect("generate response");

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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_adapter_captures_provider_triggered_compaction() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let repeated_context = "Lightspeed is testing OpenAI Responses provider-triggered compaction with encrypted native context state. This sentence is repeated to exceed the minimum compact threshold.";
    let input_text = std::iter::repeat_n(repeated_context, 300)
        .collect::<Vec<_>>()
        .join("\n");
    let input_ref = text_blob(&blobs, &input_text).await;
    let context_entry = ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(1),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        content: engine::ContentRef {
            content_ref: input_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    };
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live-compaction"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_string(),
                model: live_compaction_model(),
            },
            request_fingerprint: "live-openai-responses-compaction".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries: vec![context_entry],
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(512),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            provider_response_id: None,
            compaction: Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens: Some(2000),
            }),
            params: Some(openai_params(&OpenAiResponsesParams {
                include: vec![OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE.to_string()],
                store: Some(false),
                stream: Some(false),
                ..OpenAiResponsesParams::default()
            })),
        },
    };

    let execution = adapter.generate(request).await.expect("generate response");

    let compaction = execution
        .result
        .context_entries
        .iter()
        .find(|entry| {
            entry.content.provider_kind.as_deref()
                == Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        })
        .expect("expected provider-triggered compaction context entry");
    assert!(matches!(compaction.kind, ContextEntryKind::ProviderOpaque));
    let raw = blobs
        .read_text(&compaction.content.content_ref)
        .await
        .expect("raw compaction item");
    let raw: Value = serde_json::from_str(&raw).expect("raw compaction JSON");
    assert!(matches!(
        raw["type"].as_str(),
        Some("compaction" | "compaction_summary" | "context_compaction")
    ));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and an OpenAI Responses model that supports hosted web_search (costs real money)"]
async fn openai_responses_live_adapter_captures_web_search_call_and_citations() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let native = OpenAiResponsesWebSearchConfig {
        mode: WebSearchMode::Live,
        search_context_size: Some(WebSearchContextSize::Low),
        allowed_domains: vec!["developers.openai.com".to_string()],
        blocked_domains: Vec::new(),
        user_location: None,
        include_sources: true,
    }
    .native_tool_json()
    .expect("web search definition")
    .expect("enabled web search");

    let input_ref = text_blob(
        &blobs,
        "Use web search to find the current OpenAI Responses web_search documentation. Reply with one short sentence.",
    )
    .await;
    let context_entry = ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(1),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        content: engine::ContentRef {
            content_ref: input_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    };
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live-web-search"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_string(),
                model: live_web_search_model(),
            },
            request_fingerprint: "live-openai-responses-web-search".to_string(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiResponses,
                context_revision: 0,
                entries: vec![context_entry],
                token_estimate: None,
            },
            tools: vec![engine::ToolSpec {
                name: engine::ToolName::new("web_search"),
                kind: engine::ToolKind::ProviderNative(engine::ProviderNativeToolSpec {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    native_tool_ref: blobs
                        .put_bytes(serde_json::to_vec(&native).expect("native json"))
                        .await
                        .expect("native definition"),
                    execution: engine::ProviderNativeToolExecution::ProviderHosted,
                }),
                parallelism: engine::ToolParallelism::ParallelSafe,
                execution: Default::default(),
            }],
            tool_choice: Some(ToolChoice::RequiredAny),
            output_limit: Some(1024),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            provider_response_id: None,
            compaction: None,
            params: Some(openai_params(&OpenAiResponsesParams {
                include: vec![OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE.to_string()],
                store: Some(false),
                stream: Some(false),
                ..OpenAiResponsesParams::default()
            })),
        },
    };

    let execution = adapter.generate(request).await.expect("generate response");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert!(
        execution
            .result
            .context_entries
            .iter()
            .any(|entry| entry.content.provider_kind.as_deref()
                == Some(OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND)),
        "expected provider-opaque web_search_call context item"
    );
    let cited_index = execution
        .result
        .context_entries
        .iter()
        .position(|entry| {
            entry.content.provider_kind.as_deref() == Some(OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND)
        })
        .expect("expected native assistant message");
    assert!(matches!(
        execution.result.context_entries[cited_index].kind,
        ContextEntryKind::Message {
            role: ContextMessageRole::Assistant
        }
    ));
    let cited_entry = &execution.result.context_entries[cited_index];
    let cited_message = blobs
        .read_text(&cited_entry.content.content_ref)
        .await
        .expect("native cited message");
    let cited_message: Value =
        serde_json::from_str(&cited_message).expect("native cited message JSON");
    assert!(
        cited_message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|part| {
                part.get("annotations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .any(|annotation| annotation.get("type") == Some(&json!("url_citation"))),
        "expected native cited message to retain URL citations: {cited_message}"
    );
    let raw_response = blobs
        .read_text(&dumps(&execution).raw_response_ref)
        .await
        .expect("raw response");
    let raw_response: Value = serde_json::from_str(&raw_response).expect("raw response JSON");
    assert!(
        raw_response
            .get("output")
            .and_then(Value::as_array)
            .is_some_and(|output| output
                .iter()
                .any(|item| item.get("type") == Some(&json!("web_search_call")))),
        "expected raw response output to include web_search_call: {raw_response}"
    );
    assert!(
        raw_response
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .flat_map(|part| {
                part.get("annotations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .any(|annotation| annotation.get("type") == Some(&json!("url_citation"))),
        "expected raw response output to include url_citation annotations: {raw_response}"
    );

    // Force manual history replay (no previous_response_id) so the live API
    // also validates that the exact cited output-message item is admissible as
    // subsequent Responses input.
    let mut followup_entries = execution.result.context_entries[cited_index..=cited_index]
        .iter()
        .enumerate()
        .map(|(index, entry)| ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(index as u64 + 1),
            kind: entry.kind.clone(),
            source: ContextEntrySource::AssistantOutput {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
            },
            content: entry.content.clone(),
            preview: entry.preview.clone(),
            origin: entry.origin.clone(),
            provenance_ref: entry.provenance_ref.clone(),
            token_estimate: entry.token_estimate.clone(),
            supersedes: None,
        })
        .collect::<Vec<_>>();
    followup_entries.push(ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(3),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(2),
            input_index: 0,
        },
        content: engine::ContentRef {
            content_ref: text_blob(&blobs, "Reply with exactly: replay ok").await,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    });
    let followup = adapter
        .generate(LlmGenerationRequest {
            session_id: SessionId::new("session-live-web-search-replay"),
            run_id: RunId::new(2),
            turn_id: TurnId::new(2),
            request: LlmRequest {
                model: ModelSelection {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    provider_id: "openai".to_string(),
                    model: live_web_search_model(),
                },
                request_fingerprint: "live-openai-responses-citation-replay".to_string(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 1,
                    entries: followup_entries,
                    token_estimate: None,
                },
                tools: Vec::new(),
                tool_choice: None,
                output_limit: Some(128),
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
            },
        })
        .await
        .expect("replay cited response");
    assert_eq!(followup.result.status, LlmGenerationStatus::Succeeded);
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
async fn openai_responses_live_adapter_sees_tool_media() {
    use support::tool_media::{
        TOOL_MEDIA_PROMPT, assert_tool_media_answer, assistant_entry, retained, tool_media_entries,
        view_images_tool_spec,
    };
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, TOOL_MEDIA_PROMPT).await;
    let tool = view_images_tool_spec(&blobs).await;
    let user_entry = ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(1),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        content: engine::ContentRef::text(input_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    };
    let request = |turn: u64, entries: Vec<ContextEntry>, tool_choice: Option<ToolChoice>| {
        LlmGenerationRequest {
            session_id: SessionId::new("session-live"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(turn),
            request: LlmRequest {
                model: ModelSelection {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    provider_id: "openai".to_string(),
                    model: live_model(),
                },
                request_fingerprint: format!("live-openai-responses-tool-media-{turn}"),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 0,
                    entries,
                    token_estimate: None,
                },
                tools: vec![tool.clone()],
                tool_choice,
                output_limit: Some(2048),
                reasoning_effort: None,
                parallel_tool_use: Some(false),
                processing_tier: None,
                provider_response_id: None,
                compaction: None,
                params: Some(openai_params(&OpenAiResponsesParams {
                    store: Some(false),
                    stream: Some(false),
                    ..OpenAiResponsesParams::default()
                })),
            },
        }
    };
    let adapter = OpenAiResponsesLlmAdapter::new(
        retrying_openai_responses_client(live_client()),
        blobs.clone(),
    )
    .with_debug_dumps(true);

    let execution = adapter
        .generate(request(
            1,
            vec![user_entry.clone()],
            Some(ToolChoice::RequiredAny),
        ))
        .await
        .expect("generate tool call");
    assert_eq!(execution.result.facts.finish, LlmFinish::ToolCalls);
    let tool_call = execution
        .result
        .facts
        .tool_calls
        .first()
        .expect("observed tool call");
    assert_eq!(tool_call.tool_name.as_str(), "view_images");

    let mut entries = vec![user_entry];
    entries.extend(retained(2, &execution.result.context_entries));
    let fixture =
        tool_media_entries(&blobs, tool_call.call_id.clone(), entries.len() as u64 + 1).await;
    entries.extend(fixture.entries.clone());

    let followup_execution = adapter
        .generate(request(2, entries, Some(ToolChoice::Auto)))
        .await
        .expect("generate final answer");
    assert_eq!(
        followup_execution.result.status,
        LlmGenerationStatus::Succeeded
    );
    let final_text =
        support::content_text(blobs.as_ref(), &assistant_entry(&followup_execution)).await;
    assert_tool_media_answer(&final_text, &fixture);
}
