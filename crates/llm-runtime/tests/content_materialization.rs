use engine::{
    ContentRef, ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ContextSnapshot, LlmRequest, ModelSelection, ProviderApiKind,
    storage::{BlobStore, InMemoryBlobStore},
};
use serde_json::{Value, json};

#[tokio::test(flavor = "current_thread")]
async fn text_with_provenance_is_unchanged_across_all_provider_apis() {
    let blobs = InMemoryBlobStore::new();
    let source = blobs.put_bytes(b"source audio".to_vec()).await.unwrap();
    for expected in [
        "[audio transcript: quoted]\nKeep these spoken words.",
        r#"{"filename":"voice.ogg","text":"ordinary authored JSON"}"#,
    ] {
        let bytes = expected.as_bytes().to_vec();
        let reference = blobs.put_bytes(bytes.clone()).await.unwrap();
        for api_kind in [
            ProviderApiKind::OpenAiResponses,
            ProviderApiKind::OpenAiCompletions,
            ProviderApiKind::AnthropicMessages,
        ] {
            let entry = ContextEntry {
                entry_id: ContextEntryId::new(1),
                key: None,
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                source: ContextEntrySource::ContextEdit,
                content: ContentRef {
                    content_ref: reference.clone(),
                    media_type: Some("text/plain".into()),
                    provider_kind: None,
                },
                preview: None,
                origin: None,
                provenance_ref: Some(source.clone()),
                token_estimate: None,
                supersedes: None,
            };
            let anthropic = api_kind == ProviderApiKind::AnthropicMessages;
            let request = LlmRequest {
                model: ModelSelection {
                    api_kind: api_kind.clone(),
                    provider_id: if anthropic { "anthropic" } else { "openai" }.into(),
                    model: if anthropic {
                        "claude-sonnet-4-5"
                    } else {
                        "gpt-5.1"
                    }
                    .into(),
                },
                request_fingerprint: "content-materialization".into(),
                context: ContextSnapshot {
                    api_kind: api_kind.clone(),
                    context_revision: 1,
                    entries: vec![entry],
                    token_estimate: None,
                },
                tools: vec![],
                tool_choice: None,
                output_limit: Some(1024),
                reasoning_effort: None,
                parallel_tool_use: None,
                processing_tier: None,
                provider_response_id: None,
                compaction: None,
                params: None,
            };
            let raw: Value = match api_kind {
                ProviderApiKind::OpenAiResponses => serde_json::to_value(
                    llm_runtime::openai_responses::materialize_create_request(&blobs, &request)
                        .await
                        .unwrap(),
                )
                .unwrap(),
                ProviderApiKind::OpenAiCompletions => serde_json::to_value(
                    llm_runtime::openai_completions::materialize_create_request(&blobs, &request)
                        .await
                        .unwrap(),
                )
                .unwrap(),
                ProviderApiKind::AnthropicMessages => serde_json::to_value(
                    llm_runtime::anthropic_messages::materialize_create_request(&blobs, &request)
                        .await
                        .unwrap(),
                )
                .unwrap(),
            };
            let rendered = match api_kind {
                ProviderApiKind::OpenAiResponses => &raw["input"][0]["content"],
                ProviderApiKind::OpenAiCompletions => &raw["messages"][0]["content"],
                ProviderApiKind::AnthropicMessages => &raw["messages"][0]["content"][0]["text"],
            };
            assert_eq!(rendered, &json!(expected), "{api_kind:?}");
            assert_eq!(blobs.read_bytes(&reference).await.unwrap(), bytes);
        }
    }
}
