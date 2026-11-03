//! Provider failures through a real client and a local listener, so the wire
//! text decides the outcome and not a kind a test set by hand.

use std::sync::Arc;

use harness::{
    BlobRef, ContentRef, ContextCompactionRequest, ContextCompactionStatus, ContextCompactionTask,
    ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource, ContextMessageRole,
    ContextSnapshot, CoreAgentIoError, CoreAgentLlm as _, LlmFinish, LlmGenerationRequest,
    LlmGenerationStatus, LlmRequest, ModelSelection, ProviderApiKind, RunId, SessionId, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_clients::{
    anthropic::messages as am,
    openai::{completions as oai_c, responses as oai},
};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmGenerationAdapter, LlmGenerationExecution,
    LlmRuntime, OpenAiCompletionsLlmAdapter, OpenAiResponsesLlmAdapter,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const OVERFLOW_MESSAGE: &str = "prompt is too long: 250000 tokens > 200000 maximum";

fn overflow_body() -> Value {
    json!({
        "type": "error",
        "error": { "type": "invalid_request_error", "message": OVERFLOW_MESSAGE },
        "request_id": "req_test",
    })
}

fn summary_body(id: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": "claude-summary-test",
        "content": [{ "type": "text", "text": "the summary" }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 100, "output_tokens": 10 },
    })
}

async fn read_request(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).await.expect("read");
        assert!(read > 0, "client hung up before finishing its request");
        bytes.extend_from_slice(&buffer[..read]);
        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
        let size: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .map_or(0, |value| value.parse().expect("content length"));
        if bytes.len() >= end + 4 + size {
            return;
        }
    }
}

/// Answers one connection per scripted reply, in order.
async fn scripted_server(replies: Vec<(u16, Value)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_request(&mut stream).await;
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.expect("write");
        }
    });
    format!("http://{address}/v1")
}

fn generation_request(
    api_kind: ProviderApiKind,
    entries: Vec<ContextEntry>,
) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("provider-failures"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            code_mode: None,
            model: ModelSelection {
                api_kind: api_kind.clone(),
                provider_id: match api_kind {
                    ProviderApiKind::AnthropicMessages => "anthropic",
                    _ => "openai",
                }
                .to_owned(),
                model: "compatible-model".to_owned(),
            },
            request_fingerprint: "test".to_owned(),
            context: ContextSnapshot {
                api_kind,
                ..snapshot(entries)
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(16),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            compaction: None,
            params: None,
        },
    }
}

fn runtime(base_url: String, blobs: Arc<InMemoryBlobStore>) -> LlmRuntime {
    let mut config = am::Config::new("test-key");
    config.base_url = base_url;
    let client = Arc::new(am::Client::new(config).expect("client"));
    let adapter = Arc::new(AnthropicMessagesLlmAdapter::new(client, blobs));
    LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::AnthropicMessages, adapter.clone())
            .with_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter),
    )
}

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::AnthropicMessages,
        provider_id: "anthropic".to_owned(),
        // Outside the native compaction allowlist, so compaction summarizes.
        model: "claude-summary-test".to_owned(),
    }
}

fn user_entry(id: u64, content_ref: BlobRef) -> ContextEntry {
    ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(id),
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        source: ContextEntrySource::RunInput {
            run_id: RunId::new(id),
            input_index: 0,
        },
        content: ContentRef::text(content_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }
}

fn snapshot(entries: Vec<ContextEntry>) -> ContextSnapshot {
    ContextSnapshot {
        api_kind: ProviderApiKind::AnthropicMessages,
        context_revision: 3,
        entries,
        token_estimate: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_overflow_rejection_reaches_the_runtime_as_a_context_limit() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("hello").await;
    let base_url = scripted_server(vec![(400, overflow_body())]).await;
    let error = runtime(base_url, blobs)
        .generate(generation_request(
            ProviderApiKind::AnthropicMessages,
            vec![user_entry(1, content)],
        ))
        .await
        .expect_err("the rejection is an error");
    assert_eq!(
        error,
        CoreAgentIoError::ContextLimit {
            message: OVERFLOW_MESSAGE.to_owned()
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_overflow_rejection_makes_rolling_compaction_split_its_chunk() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("some earlier history").await;
    let entries: Vec<_> = (1..=2).map(|id| user_entry(id, content.clone())).collect();
    let base_url = scripted_server(vec![
        (400, overflow_body()),
        (200, summary_body("msg_first")),
        (200, summary_body("msg_second")),
    ])
    .await;
    let result = runtime(base_url, blobs)
        .compact_context(ContextCompactionRequest {
            session_id: SessionId::new("overflow"),
            request: ContextCompactionTask {
                model: model(),
                request_fingerprint: "test".to_owned(),
                context: snapshot(entries),
                target_tokens: None,
                tools: Vec::new(),
                input_limit_tokens: None,
                covered_entry_ids: (1..=2).map(ContextEntryId::new).collect(),
            },
        })
        .await
        .expect("the split chunks fit");
    assert_eq!(result.status, ContextCompactionStatus::Succeeded);
    assert_eq!(result.calls, 3, "the refused whole chunk, then both halves");
    assert_eq!(result.context_entries.len(), 1);
}

/// The turn ended as a normal failed generation with what it cost, the reason in
/// its failure text and nothing of the response in context.
async fn assert_unreadable_response_failed(
    blobs: &InMemoryBlobStore,
    execution: LlmGenerationExecution,
    expected_id: &str,
    expected_input_tokens: u32,
    expected_text: &str,
) {
    let result = execution.result;
    assert_eq!(result.status, LlmGenerationStatus::Failed);
    assert_eq!(result.facts.finish, LlmFinish::Failed);
    assert_eq!(
        result.facts.provider_response_id.as_deref(),
        Some(expected_id)
    );
    assert_eq!(
        result.facts.usage.expect("usage survives").input_tokens,
        Some(expected_input_tokens)
    );
    assert!(result.context_entries.is_empty());
    assert!(result.facts.tool_calls.is_empty());
    let failure_ref = result.failure_ref.expect("failure text");
    let failure = blobs.read_bytes(&failure_ref).await.expect("failure blob");
    let failure = String::from_utf8(failure).expect("UTF-8");
    assert!(
        failure.contains("invalid provider response"),
        "the error says the response was bad: {failure}"
    );
    assert!(failure.contains(expected_text), "{failure}");
    let dumps = execution.debug_dumps.expect("debug dump survives");
    let raw = blobs
        .read_bytes(&dumps.raw_response_ref)
        .await
        .expect("raw response blob");
    assert!(
        String::from_utf8_lossy(&raw).contains(expected_id),
        "the dump holds the raw response"
    );
}

fn completion_with_call(id: Value, name: &str) -> Value {
    json!({
        "id": "chatcmpl_bad",
        "object": "chat.completion",
        "created": 1,
        "model": "compatible-model",
        "choices": [{
            "index": 0,
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": "sibling text",
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": "{}" },
                }],
            },
        }],
        "usage": { "prompt_tokens": 40, "completion_tokens": 5, "total_tokens": 45 },
    })
}

async fn completions_outcome(body: Value) -> (Arc<InMemoryBlobStore>, LlmGenerationExecution) {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("hello").await;
    let mut config = oai_c::Config::new("test-key");
    config.base_url = scripted_server(vec![(200, body)]).await;
    let adapter = OpenAiCompletionsLlmAdapter::new(
        Arc::new(oai_c::Client::new(config).expect("client")),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let execution = adapter
        .generate(generation_request(
            ProviderApiKind::OpenAiCompletions,
            vec![user_entry(1, content)],
        ))
        .await
        .expect("an unreadable response is a failed generation, not an error");
    (blobs, execution)
}

#[tokio::test(flavor = "current_thread")]
async fn completions_identities_outside_the_grammar_fail_the_turn_and_keep_its_usage() {
    for (id, name, expected_text) in [
        (
            json!("call_1"),
            "bash<|channel|>commentary",
            "invalid Chat Completions tool name",
        ),
        (
            json!(""),
            "read_file",
            "invalid Chat Completions tool call id",
        ),
        (
            json!("5x450f94__thought__EvACCu0+/="),
            "read_file",
            "invalid Chat Completions tool call id",
        ),
        (
            Value::Null,
            "read_file",
            "Chat Completions tool call is missing id",
        ),
    ] {
        let (blobs, execution) = completions_outcome(completion_with_call(id, name)).await;
        assert_unreadable_response_failed(&blobs, execution, "chatcmpl_bad", 40, expected_text)
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_well_formed_unknown_name_still_reaches_the_unavailable_path() {
    let (_, execution) =
        completions_outcome(completion_with_call(json!("call_1"), "functions.read_file")).await;
    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    let call = &execution.result.facts.tool_calls[0];
    assert_eq!(call.tool_name.as_str(), "functions.read_file");
    assert_eq!(call.tool_id, None, "no tool answers to this name");
}

#[tokio::test(flavor = "current_thread")]
async fn responses_identities_outside_the_grammar_fail_the_turn_and_keep_its_usage() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("hello").await;
    let mut config = oai::Config::new("test-key");
    config.base_url = scripted_server(vec![(
        200,
        json!({
            "id": "resp_bad",
            "object": "response",
            "status": "completed",
            "model": "compatible-model",
            "output": [{
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash<|channel|>commentary",
                "arguments": "{}",
            }],
            "usage": { "input_tokens": 41, "output_tokens": 5, "total_tokens": 46 },
        }),
    )])
    .await;
    let adapter = OpenAiResponsesLlmAdapter::new(
        Arc::new(oai::Client::new(config).expect("client")),
        blobs.clone(),
    )
    .with_debug_dumps(true);
    let execution = adapter
        .generate(generation_request(
            ProviderApiKind::OpenAiResponses,
            vec![user_entry(1, content)],
        ))
        .await
        .expect("an unreadable response is a failed generation, not an error");
    assert_unreadable_response_failed(
        &blobs,
        execution,
        "resp_bad",
        41,
        "invalid OpenAI tool name",
    )
    .await;
}

fn anthropic_message(id: &str, stop_reason: &str, content: Value) -> Value {
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": "compatible-model",
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": { "input_tokens": 30, "output_tokens": 5 },
    })
}

fn anthropic_adapter(
    base_url: String,
    blobs: Arc<InMemoryBlobStore>,
) -> AnthropicMessagesLlmAdapter {
    let mut config = am::Config::new("test-key");
    config.base_url = base_url;
    AnthropicMessagesLlmAdapter::new(Arc::new(am::Client::new(config).expect("client")), blobs)
        .with_debug_dumps(true)
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_keeps_the_usage_of_every_continuation_when_the_last_response_is_unreadable() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("hello").await;
    let base_url = scripted_server(vec![
        (
            200,
            anthropic_message(
                "msg_paused",
                "pause_turn",
                json!([{ "type": "text", "text": "still working" }]),
            ),
        ),
        (
            200,
            anthropic_message(
                "msg_bad",
                "tool_use",
                json!([{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "bash<|channel|>commentary",
                    "input": {},
                }]),
            ),
        ),
    ])
    .await;
    let execution = anthropic_adapter(base_url, blobs.clone())
        .generate(generation_request(
            ProviderApiKind::AnthropicMessages,
            vec![user_entry(1, content)],
        ))
        .await
        .expect("an unreadable response is a failed generation, not an error");
    assert_unreadable_response_failed(
        &blobs,
        execution,
        "msg_bad",
        60,
        "invalid Anthropic tool name",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_keeps_the_usage_of_a_turn_that_never_stops_pausing() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("hello").await;
    let paused = anthropic_message(
        "msg_paused",
        "pause_turn",
        json!([{ "type": "text", "text": "still working" }]),
    );
    let base_url = scripted_server(vec![(200, paused); 9]).await;
    let execution = anthropic_adapter(base_url, blobs.clone())
        .generate(generation_request(
            ProviderApiKind::AnthropicMessages,
            vec![user_entry(1, content)],
        ))
        .await
        .expect("an unreadable response is a failed generation, not an error");
    assert_unreadable_response_failed(&blobs, execution, "msg_paused", 270, "pause_turn").await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_the_adapter_cannot_build_stays_a_request_error() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let content = blobs.insert_text("hello").await;
    let mut request = generation_request(
        ProviderApiKind::OpenAiCompletions,
        vec![user_entry(1, content)],
    );
    request.request.reasoning_effort = Some("ultra".to_owned());
    let mut config = oai_c::Config::new("test-key");
    config.base_url = refused_base_url().await;
    let adapter = OpenAiCompletionsLlmAdapter::new(
        Arc::new(oai_c::Client::new(config).expect("client")),
        blobs,
    );
    let Err(error) = adapter.generate(request).await else {
        panic!("an unknown effort cannot be sent");
    };
    assert!(
        matches!(
            error,
            llm_runtime::LlmAdapterError::InvalidProviderRequest { .. }
        ),
        "{error}"
    );
}

async fn refused_base_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    format!("http://{address}/v1")
}
