use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ContextSnapshot, LlmFinish, LlmGenerationRequest, LlmGenerationStatus,
    LlmRequest, LlmRequestKind, ModelProviderOptions, ModelSelection, OpenAiResponsesRequest,
    ProviderApiKind, RunId, SessionId, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_clients::openai::responses::{Client, Config};
use llm_runtime::{LlmGenerationAdapter, OpenAiResponsesLlmAdapter};

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5-mini".to_string())
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_adapter_generates_result() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let input_ref = text_blob(&blobs, "Reply with exactly these two words: forge adapter").await;
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
        content_ref: input_ref,
        media_type: None,
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    };
    let adapter = OpenAiResponsesLlmAdapter::new(Arc::new(live_client()), blobs.clone());
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-live"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_string(),
                model: live_model(),
                options: ModelProviderOptions::None,
            },
            request_fingerprint: "live-openai-responses".to_string(),
            kind: LlmRequestKind::OpenAiResponses(OpenAiResponsesRequest {
                input_context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 0,
                    entries: vec![context_entry],
                    token_estimate: None,
                },
                previous_response_id: None,
                tools: Vec::new(),
                tool_choice: None,
                reasoning: None,
                text: None,
                include: Vec::new(),
                max_output_tokens: Some(512),
                max_tool_calls: None,
                temperature: None,
                top_p: None,
                metadata: BTreeMap::new(),
                parallel_tool_calls: None,
                store: Some(false),
                stream: Some(false),
                truncation: None,
                context_management: None,
                extra: BTreeMap::new(),
            }),
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
