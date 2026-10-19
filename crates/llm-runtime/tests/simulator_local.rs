//! Explicit local simulator integration; never resolves deployment credentials.
use harness::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ContextSnapshot, LlmFinish, LlmGenerationRequest, LlmGenerationStatus,
    LlmRequest, ModelSelection, ProviderApiKind, RunId, SessionId, TurnId,
    storage::InMemoryBlobStore,
};
use llm_runtime::{
    LlmGenerationAdapter, OpenAiCompletionsLlmAdapter, ResolvedEndpoint, ResolvedModelProvider,
    StaticModelProviders,
};
use std::{collections::BTreeMap, sync::Arc};
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
        content: harness::ContentRef {
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
        session_id: SessionId::new("session-local-simulator"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            code_mode: None,
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiCompletions,
                provider_id: "perf".into(),
                model: "perf-text".into(),
            },
            request_fingerprint: "local-simulator".to_owned(),
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
            params: None,
        },
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local llm-simulator on 127.0.0.1:19090 with scripts/perf/fixtures/simulator.json"]
async fn simulator_response_runs_through_native_adapter_and_cas() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let user = blobs.insert_text("synthetic local integration input").await;
    let request = generation_request(vec![entry(
        1,
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        ContextEntrySource::RunInput {
            run_id: RunId::new(1),
            input_index: 0,
        },
        user,
    )]);
    let endpoint = ResolvedEndpoint::new(
        "http://127.0.0.1:19090/v1",
        &BTreeMap::new(),
        ["openai:completions".to_owned()],
    )
    .unwrap();
    let providers = StaticModelProviders::new().with_provider(
        "perf",
        ResolvedModelProvider {
            auth: None,
            endpoint: Some(endpoint),
        },
    );
    let client = llm_clients::openai::completions::Client::new(
        llm_clients::openai::completions::Config::without_api_key(),
    )
    .unwrap();
    let adapter = OpenAiCompletionsLlmAdapter::new(Arc::new(client), blobs)
        .with_provider_key_resolver(Arc::new(providers));
    let result = adapter.generate(request).await.unwrap().result;
    assert_eq!(result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(result.facts.finish, LlmFinish::Stop);
    let usage = result.facts.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(100));
    assert_eq!(usage.output_tokens, Some(64));
    assert!(!result.context_entries.is_empty());
}
