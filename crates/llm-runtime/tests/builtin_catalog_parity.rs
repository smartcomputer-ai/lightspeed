use engine::{
    ContextSnapshot, LlmRequest, ModelSelection, ProviderApiKind, ToolChoice, ToolSpec,
    storage::InMemoryBlobStore,
};
use serde_json::Value;
use tools::{
    builtin::{BuiltinTool, BuiltinToolOperation},
    toolset::{BuiltinToolPresentation, EnvironmentToolsetConfig, ToolsetConfig},
};

fn config(case: &str, api: &ProviderApiKind) -> ToolsetConfig {
    let mut config = ToolsetConfig::empty();
    match case {
        "workspace" => config = ToolsetConfig::workspace(),
        "environment" | "one_shot" | "canonical" => {
            config.builtin.environment = EnvironmentToolsetConfig::basic();
            config.environment_read = true;
            config.environment_selection = true;
            if case == "one_shot" {
                config.builtin.environment.continue_process = false;
            }
            if case == "canonical" {
                config.builtin.presentation = BuiltinToolPresentation::Canonical;
            }
        }
        "web" => {
            config.web.fetch = true;
            if *api != ProviderApiKind::OpenAiCompletions {
                config.web.search = Some(tools::web::search::WebSearchToolConfig::new(
                    vec!["example.com".into()],
                    Vec::new(),
                ));
            }
        }
        "workflow" => {
            config.concurrency.enabled = true;
            config.concurrency.timer = true;
        }
        _ => unreachable!(),
    }
    config
}

fn registered(case: &str, api: &ProviderApiKind) -> Vec<ToolSpec> {
    let mut registered =
        tools::toolset::register_toolset(&config(case, api)).expect("registrations");
    if case == "workflow" {
        for id in ["subagent.run", "subagent.spawn"] {
            let tool = tools::definitions::register(
                id,
                Default::default(),
                engine::ToolParallelism::ParallelSafe,
                Default::default(),
            );
            registered.tools.insert(tool.name.clone(), tool);
        }
        for operation in [
            BuiltinToolOperation::JobSubmit,
            BuiltinToolOperation::JobRun,
        ] {
            let builtin = BuiltinTool::environment_canonical(operation);
            let tool = tools::definitions::register(
                builtin.logical_id(),
                tools::definitions::BuiltinSettings {
                    presentation: BuiltinToolPresentation::Canonical,
                    unscoped_paths: true,
                    ..Default::default()
                },
                builtin.parallelism(),
                builtin.execution_spec(),
            );
            registered.tools.insert(tool.name.clone(), tool);
        }
    }
    registered.tools.into_values().collect()
}

async fn fixture(api: ProviderApiKind, case: &str) -> Value {
    let blobs = InMemoryBlobStore::new();
    let model = ModelSelection {
        provider_id: if api == ProviderApiKind::AnthropicMessages {
            "anthropic"
        } else {
            "openai"
        }
        .into(),
        model: if api == ProviderApiKind::AnthropicMessages {
            "claude-opus-4-8"
        } else {
            "gpt-5.1"
        }
        .into(),
        api_kind: api.clone(),
    };
    let tools = registered(case, &api);
    let request = LlmRequest {
        model,
        request_fingerprint: "sha256:catalog-parity".into(),
        context: ContextSnapshot {
            api_kind: api.clone(),
            context_revision: 0,
            entries: Vec::new(),
            token_estimate: None,
        },
        tools,
        tool_choice: Some(ToolChoice::Auto),
        output_limit: Some(4096),
        reasoning_effort: None,
        parallel_tool_use: Some(true),
        processing_tier: None,
        provider_response_id: None,
        compaction: None,
        params: None,
    };
    match api {
        ProviderApiKind::OpenAiResponses => serde_json::to_value(
            llm_runtime::openai_responses::materialize_create_request(&blobs, &request)
                .await
                .expect("responses"),
        ),
        ProviderApiKind::AnthropicMessages => serde_json::to_value(
            llm_runtime::anthropic_messages::materialize_create_request(&blobs, &request)
                .await
                .expect("messages"),
        ),
        ProviderApiKind::OpenAiCompletions => serde_json::to_value(
            llm_runtime::openai_completions::materialize_create_request(&blobs, &request)
                .await
                .expect("completions"),
        ),
    }
    .expect("request json")
}

/// Captured provider contracts track intentional changes to the builtin surface.
/// Compare complete requests: descriptions, schemas, strictness, order, helper
/// placement, and cache breakpoints. The resolver runs with an empty blob store.
#[tokio::test(flavor = "current_thread")]
async fn builtin_requests_match_captured_provider_contracts() {
    let baseline: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/builtin_catalogs.json")).expect("baseline");
    for entry in baseline {
        let api: ProviderApiKind = serde_json::from_value(entry["api"].clone()).expect("API kind");
        let case = entry["case"].as_str().expect("case");
        let actual = fixture(api.clone(), case).await;
        assert_eq!(actual, entry["request"], "{api:?}: {case}");
    }
}
