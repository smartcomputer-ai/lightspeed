use harness::{
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
            let raw = materialized(&blobs, &api_kind, entry).await;
            assert_eq!(rendered(&raw, &api_kind), &json!(expected), "{api_kind:?}");
            assert_eq!(blobs.read_bytes(&reference).await.unwrap(), bytes);
        }
    }
}

const ALL_APIS: [(ProviderApiKind, &str); 3] = [
    (
        ProviderApiKind::OpenAiResponses,
        "openai.responses.compaction_summary_text",
    ),
    (
        ProviderApiKind::OpenAiCompletions,
        harness::OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND,
    ),
    (
        ProviderApiKind::AnthropicMessages,
        harness::ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND,
    ),
];

async fn materialized(
    blobs: &InMemoryBlobStore,
    api_kind: &ProviderApiKind,
    entry: ContextEntry,
) -> Value {
    let anthropic = *api_kind == ProviderApiKind::AnthropicMessages;
    let request = LlmRequest {
        code_mode: None,
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
        compaction: None,
        params: None,
    };
    match api_kind {
        ProviderApiKind::OpenAiResponses => serde_json::to_value(
            llm_runtime::openai_responses::materialize_create_request(blobs, &request)
                .await
                .unwrap(),
        )
        .unwrap(),
        ProviderApiKind::OpenAiCompletions => serde_json::to_value(
            llm_runtime::openai_completions::materialize_create_request(blobs, &request)
                .await
                .unwrap(),
        )
        .unwrap(),
        ProviderApiKind::AnthropicMessages => serde_json::to_value(
            llm_runtime::anthropic_messages::materialize_create_request(blobs, &request)
                .await
                .unwrap(),
        )
        .unwrap(),
    }
}

fn rendered<'a>(raw: &'a Value, api_kind: &ProviderApiKind) -> &'a Value {
    match api_kind {
        ProviderApiKind::OpenAiResponses => &raw["input"][0]["content"],
        ProviderApiKind::OpenAiCompletions => &raw["messages"][0]["content"],
        ProviderApiKind::AnthropicMessages => &raw["messages"][0]["content"][0]["text"],
    }
}

fn message_entry(
    role: ContextMessageRole,
    content_ref: harness::BlobRef,
    provider_kind: Option<&str>,
    preview: &str,
) -> ContextEntry {
    ContextEntry {
        entry_id: ContextEntryId::new(1),
        key: None,
        kind: ContextEntryKind::Message { role },
        source: ContextEntrySource::ContextEdit,
        content: ContentRef {
            content_ref,
            media_type: Some("text/plain".into()),
            provider_kind: provider_kind.map(str::to_owned),
        },
        preview: Some(preview.to_owned()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_summary_opening_like_a_document_lowers_as_ordinary_text() {
    let blobs = InMemoryBlobStore::new();
    let summary = "[document: report.pdf] was reviewed and the numbers were corrected.";
    let content_ref = blobs.insert_text(summary).await;
    for (api_kind, provider_kind) in ALL_APIS {
        let entry = message_entry(
            ContextMessageRole::User,
            content_ref.clone(),
            Some(provider_kind),
            summary,
        );
        let raw = materialized(&blobs, &api_kind, entry).await;
        assert_eq!(rendered(&raw, &api_kind), &json!(summary), "{api_kind:?}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_truncated_answer_opening_like_a_document_lowers_as_ordinary_text() {
    let blobs = InMemoryBlobStore::new();
    let partial = "[document] is what the user sent, and it says";
    let content_ref = blobs.insert_text(partial).await;
    for (api_kind, _) in ALL_APIS {
        let entry = message_entry(
            ContextMessageRole::Assistant,
            content_ref.clone(),
            None,
            partial,
        );
        let raw = materialized(&blobs, &api_kind, entry).await;
        assert_eq!(rendered(&raw, &api_kind), &json!(partial), "{api_kind:?}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_admitted_text_document_still_lowers_as_a_document() {
    let blobs = InMemoryBlobStore::new();
    let content_ref = blobs.insert_text("the notes").await;
    let entry = message_entry(
        ContextMessageRole::User,
        content_ref,
        None,
        "[document: notes.txt]",
    );
    let raw = materialized(&blobs, &ProviderApiKind::AnthropicMessages, entry).await;
    let block = &raw["messages"][0]["content"][0];
    assert_eq!(block["type"], json!("document"), "{block}");
    assert_eq!(block["title"], json!("notes.txt"), "{block}");
}
