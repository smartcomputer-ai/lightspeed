//! Provider failures through a real client and a local listener, so the wire
//! text decides the outcome and not a kind a test set by hand.

use std::sync::Arc;

use harness::{
    BlobRef, ContentRef, ContextCompactionRequest, ContextCompactionStatus, ContextCompactionTask,
    ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource, ContextMessageRole,
    ContextSnapshot, CoreAgentIoError, CoreAgentLlm as _, LlmGenerationRequest, LlmRequest,
    ModelSelection, ProviderApiKind, RunId, SessionId, TurnId, storage::InMemoryBlobStore,
};
use llm_clients::anthropic::messages as am;
use llm_runtime::{AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime};
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
        .generate(LlmGenerationRequest {
            session_id: SessionId::new("overflow"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: LlmRequest {
                code_mode: None,
                model: model(),
                request_fingerprint: "test".to_owned(),
                context: snapshot(vec![user_entry(1, content)]),
                tools: Vec::new(),
                tool_choice: None,
                output_limit: Some(16),
                reasoning_effort: None,
                parallel_tool_use: None,
                processing_tier: None,
                compaction: None,
                params: None,
            },
        })
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
