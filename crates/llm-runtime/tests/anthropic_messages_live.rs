use std::path::PathBuf;
use std::sync::Arc;

use engine::{
    ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND, BlobRef, ContextCompactionRequest,
    ContextCompactionStatus, ContextCompactionTask, ContextEntry, ContextEntryId,
    ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextMessageRole, ContextSnapshot,
    LlmFinish, LlmGenerationRequest, LlmGenerationStatus, LlmRequest, ModelSelection,
    ProviderApiKind, RunId, SessionId, ToolChoice, ToolChoiceMode, ToolName, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_clients::anthropic::messages::{Client, Config};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmCompactionAdapter, LlmGenerationAdapter,
    params::{AnthropicMessagesParams, AnthropicThinkingConfig},
};
use serde_json::json;

mod support;

use support::{anthropic_params, retrying_anthropic_messages_client};

fn live_model() -> String {
    env_or_dotenv_var("ANTHROPIC_MESSAGES_MODEL")
        .or_else(|_| env_or_dotenv_var("ANTHROPIC_LIVE_MODEL"))
        .unwrap_or_else(|_| "claude-opus-4-8".to_string())
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
        content_ref,
        media_type: None,
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
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
        output_limit: Some(1024),
        provider_response_id: None,
        compaction: None,
        params: None,
    }
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
        content_ref: item.content_ref.clone(),
        media_type: item.media_type.clone(),
        preview: item.preview.clone(),
        provider_kind: item.provider_kind.clone(),
        provider_item_id: item.provider_item_id.clone(),
        token_estimate: item.token_estimate.clone(),
    }
}

fn weather_tool_spec(schema_ref: BlobRef, description_ref: BlobRef) -> engine::ToolSpec {
    engine::ToolSpec {
        name: ToolName::new("get_weather"),
        kind: engine::ToolKind::Function(engine::FunctionToolSpec {
            model_name: None,
            description_ref: Some(description_ref),
            input_schema_ref: schema_ref,
            output_schema_ref: None,
            strict: None,
            provider_options_ref: None,
        }),
        parallelism: engine::ToolParallelism::ParallelSafe,
        target_requirement: engine::ToolTargetRequirement::None,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_generates_result() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, "Reply with exactly these two words: forge adapter").await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    );
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
            } => Some(item.content_ref.clone()),
            _ => None,
        })
        .expect("assistant context item");
    let assistant_text = blobs
        .read_text(&assistant_ref)
        .await
        .expect("assistant text");
    assert!(
        assistant_text.to_lowercase().contains("forge"),
        "expected assistant output to contain forge, got {assistant_text:?}"
    );

    let provider_request = blobs
        .read_text(&execution.provider_request_ref)
        .await
        .expect("provider request blob");
    assert!(
        provider_request.contains("\"model\""),
        "expected provider request JSON, got {provider_request}"
    );
    let raw_response = blobs
        .read_text(&execution.raw_response_ref)
        .await
        .expect("raw response blob");
    assert!(
        raw_response.contains("\"id\""),
        "expected raw response JSON, got {raw_response}"
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
    );

    let mut request = intent_request(
        "live-anthropic-messages-tool",
        vec![user_entry(1, input_ref.clone())],
    );
    request.tools = vec![weather_tool_spec(schema_ref.clone(), description_ref.clone())];
    request.tool_choice = Some(ToolChoice {
        mode: ToolChoiceMode::RequiredAny,
        disable_parallel_tool_use: Some(true),
    });

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
        content_ref: tool_output_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
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
            } => Some(item.content_ref.clone()),
            _ => None,
        })
        .expect("final assistant context item");
    let final_text = blobs.read_text(&final_ref).await.expect("final text");
    assert!(
        final_text.contains("11"),
        "expected final answer to use the tool result, got {final_text:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_adapter_preserves_thinking_blocks() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(
        &blobs,
        "Compute 13 * 17 and 29 * 31, then their sum. Reply with just the final number.",
    )
    .await;
    let adapter = AnthropicMessagesLlmAdapter::new(
        retrying_anthropic_messages_client(live_client()),
        blobs.clone(),
    );

    let mut request = intent_request(
        "live-anthropic-messages-thinking",
        vec![user_entry(1, input_ref.clone())],
    );
    request.output_limit = Some(4096);
    request.params = Some(anthropic_params(&AnthropicMessagesParams {
        thinking: Some(AnthropicThinkingConfig {
            r#type: "adaptive".to_string(),
            budget_tokens: None,
            display: None,
            extra: Default::default(),
        }),
        output_config: Some(json!({ "effort": "high" })),
        ..AnthropicMessagesParams::default()
    }));

    let execution = adapter
        .generate(generation_request(1, request.clone()))
        .await
        .expect("generate with thinking");

    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    assert!(
        execution
            .result
            .context_entries
            .iter()
            .any(|entry| matches!(entry.kind, ContextEntryKind::ReasoningState)),
        "expected a reasoning state context entry from thinking blocks"
    );
    let answer_ref = execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content_ref.clone()),
            _ => None,
        })
        .expect("assistant answer");
    let answer = blobs.read_text(&answer_ref).await.expect("answer text");
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
    followup.params = request.params;

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
            } => Some(item.content_ref.clone()),
            _ => None,
        })
        .expect("follow-up answer");
    let followup_answer = blobs
        .read_text(&followup_answer_ref)
        .await
        .expect("follow-up text");
    assert!(
        followup_answer.contains("1124"),
        "expected 1124, got {followup_answer:?}"
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
    );
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
        entry.provider_kind.as_deref(),
        Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
    );
    let summary = blobs
        .read_text(&entry.content_ref)
        .await
        .expect("summary text");
    assert!(
        summary.to_uppercase().contains("ZEPHYR"),
        "expected the summary to retain the codename, got {summary:?}"
    );
}
