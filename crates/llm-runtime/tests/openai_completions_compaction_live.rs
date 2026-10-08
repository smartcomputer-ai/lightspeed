use std::{collections::BTreeMap, sync::Arc};

use harness::{
    ContextCompactionRequest, ContextCompactionStatus, ContextCompactionTask, ContextEntry,
    ContextEntryId, ContextEntryKind, ContextEntrySource, ContextMessageRole, ContextSnapshot,
    LlmGenerationRequest, LlmGenerationStatus, LlmRequest, ModelSelection,
    OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND, ProviderApiKind, RunId, SessionId, TurnId,
    storage::InMemoryBlobStore,
};
use llm_runtime::{
    LlmCompactionAdapter, LlmGenerationAdapter, OpenAiCompletionsLlmAdapter,
    OpenAiCompletionsParams, ResolvedEndpoint, ResolvedModelProvider, ResolvedProviderAuth,
    StaticModelProviders,
};

mod support;

use support::{
    deepseek_completions_live_model, env_or_dotenv_var, openai_completions_live_client,
    openai_completions_live_model, openai_completions_params, retrying_openai_completions_client,
};

fn model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiCompletions,
        provider_id: "openai".to_owned(),
        model: openai_completions_live_model(),
    }
}

fn entry(
    id: u64,
    role: ContextMessageRole,
    source: ContextEntrySource,
    content_ref: harness::BlobRef,
) -> ContextEntry {
    ContextEntry {
        entry_id: ContextEntryId::new(id),
        key: None,
        kind: ContextEntryKind::Message { role },
        source,
        content: harness::ContentRef::text(content_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }
}

fn adapter(blobs: Arc<InMemoryBlobStore>) -> OpenAiCompletionsLlmAdapter {
    OpenAiCompletionsLlmAdapter::new(
        retrying_openai_completions_client(openai_completions_live_client()),
        blobs,
    )
}

async fn conversation(blobs: &InMemoryBlobStore) -> ContextSnapshot {
    let user_1 = blobs
        .insert_text("My project codename is Silver Kestrel. Remember it.")
        .await;
    let assistant_1 = blobs
        .insert_text("Understood. The project codename is Silver Kestrel.")
        .await;
    let user_2 = blobs
        .insert_text("We chose Rust for the runtime and PostgreSQL for durable storage.")
        .await;
    let assistant_2 = blobs
        .insert_text("I will retain those architecture decisions.")
        .await;
    ContextSnapshot {
        api_kind: ProviderApiKind::OpenAiCompletions,
        context_revision: 11,
        entries: vec![
            entry(
                1,
                ContextMessageRole::User,
                ContextEntrySource::RunInput {
                    run_id: RunId::new(1),
                    input_index: 0,
                },
                user_1,
            ),
            entry(
                2,
                ContextMessageRole::Assistant,
                ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
                assistant_1,
            ),
            entry(
                3,
                ContextMessageRole::User,
                ContextEntrySource::RunInput {
                    run_id: RunId::new(2),
                    input_index: 0,
                },
                user_2,
            ),
            entry(
                4,
                ContextMessageRole::Assistant,
                ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(2),
                    turn_id: TurnId::new(1),
                },
                assistant_2,
            ),
        ],
        token_estimate: None,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_standalone_compaction_preserves_facts() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    standalone_compaction_preserves_facts(blobs.clone(), model(), adapter(blobs)).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires DEEPSEEK_API_KEY (costs real money)"]
async fn deepseek_completions_runtime_live_standalone_compaction_preserves_facts() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    standalone_compaction_preserves_facts(blobs.clone(), deepseek_model(), deepseek_adapter(blobs))
        .await;
}

async fn standalone_compaction_preserves_facts(
    blobs: Arc<InMemoryBlobStore>,
    model: ModelSelection,
    adapter: OpenAiCompletionsLlmAdapter,
) {
    eprintln!(
        "standalone compaction: provider={}, model={}",
        model.provider_id, model.model
    );
    let params = (model.provider_id == "openai").then(|| {
        openai_completions_params(&OpenAiCompletionsParams {
            store: Some(false),
            ..Default::default()
        })
    });
    let task = ContextCompactionTask {
        covered_entry_ids: Vec::new(),
        tools: Vec::new(),
        input_limit_tokens: None,
        model,
        request_fingerprint: "openai-completions-live-compact".to_owned(),
        context: conversation(&blobs).await,
        target_tokens: Some(256),
        params,
    };

    let result = adapter
        .compact_context(ContextCompactionRequest {
            session_id: SessionId::new("session-openai-completions-compact"),
            request: task,
        })
        .await
        .expect("compact context");

    assert_eq!(result.status, ContextCompactionStatus::Succeeded);
    assert_eq!(result.context_revision, 11);
    assert_eq!(result.context_entries.len(), 1);
    assert_eq!(
        result.context_entries[0].content.provider_kind.as_deref(),
        Some(OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND)
    );
    let summary = support::content_text(blobs.as_ref(), &result.context_entries[0].content)
        .await
        .to_lowercase();
    assert!(summary.contains("silver kestrel"), "summary: {summary}");
    assert!(summary.contains("rust"), "summary: {summary}");
    assert!(summary.contains("postgres"), "summary: {summary}");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_compacted_summary_continues_conversation() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    compacted_summary_continues_conversation(blobs.clone(), model(), adapter(blobs)).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires DEEPSEEK_API_KEY (costs real money)"]
async fn deepseek_completions_runtime_live_compacted_summary_continues_conversation() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    compacted_summary_continues_conversation(
        blobs.clone(),
        deepseek_model(),
        deepseek_adapter(blobs),
    )
    .await;
}

async fn compacted_summary_continues_conversation(
    blobs: Arc<InMemoryBlobStore>,
    model: ModelSelection,
    adapter: OpenAiCompletionsLlmAdapter,
) {
    eprintln!(
        "compaction continuation: provider={}, model={}",
        model.provider_id, model.model
    );
    let task = ContextCompactionTask {
        covered_entry_ids: Vec::new(),
        tools: Vec::new(),
        input_limit_tokens: None,
        model: model.clone(),
        request_fingerprint: "openai-completions-live-compact-continue".to_owned(),
        context: conversation(&blobs).await,
        target_tokens: Some(192),
        params: None,
    };
    let compacted = adapter
        .compact_context(ContextCompactionRequest {
            session_id: SessionId::new("session-openai-completions-compact-continue"),
            request: task,
        })
        .await
        .expect("compact context");
    assert_eq!(compacted.status, ContextCompactionStatus::Succeeded);
    assert_eq!(compacted.context_entries.len(), 1);
    let summary_input = &compacted.context_entries[0];
    let summary = entry(
        1,
        ContextMessageRole::User,
        ContextEntrySource::Runtime {
            label: "compaction".to_owned(),
        },
        summary_input.content.content_ref.clone(),
    );
    let question_ref = blobs
        .insert_text("What is the project codename? Reply with only the codename.")
        .await;
    let question = entry(
        2,
        ContextMessageRole::User,
        ContextEntrySource::RunInput {
            run_id: RunId::new(3),
            input_index: 0,
        },
        question_ref,
    );
    let request = LlmGenerationRequest {
        session_id: SessionId::new("session-openai-completions-compact-continue"),
        run_id: RunId::new(3),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            code_mode: None,
            model,
            request_fingerprint: "openai-completions-live-after-compact".to_owned(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiCompletions,
                context_revision: 12,
                entries: vec![summary, question],
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
            params: None,
        },
    };

    let execution = adapter
        .generate(request)
        .await
        .expect("continue from compacted context");
    assert_eq!(execution.result.status, LlmGenerationStatus::Succeeded);
    let answer =
        support::content_text(blobs.as_ref(), &execution.result.context_entries[0].content)
            .await
            .to_lowercase();

    assert!(answer.contains("silver kestrel"), "answer: {answer}");
}

fn deepseek_model() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiCompletions,
        provider_id: "deepseek".to_owned(),
        model: deepseek_completions_live_model(),
    }
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
    let client = llm_clients::openai::completions::Client::new(
        llm_clients::openai::completions::Config::without_api_key(),
    )
    .expect("base completion client");
    OpenAiCompletionsLlmAdapter::new(retrying_openai_completions_client(client), blobs)
        .with_provider_key_resolver(Arc::new(providers))
}
