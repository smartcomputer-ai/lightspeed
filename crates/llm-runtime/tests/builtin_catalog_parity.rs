use harness::{
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
                harness::ToolParallelism::ParallelSafe,
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

async fn fixture(api: ProviderApiKind, case: &str, code_mode: bool) -> Value {
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
    let mut tools = registered(case, &api);
    let code_mode = code_mode.then(|| {
        let presentation = harness::CodeModePresentation {
            allowed_tools: tools.iter().map(|tool| tool.name.clone()).collect(),
            ..Default::default()
        };
        tools.push(tools::definitions::register(
            "code.execute",
            Default::default(),
            harness::ToolParallelism::ParallelSafe,
            Default::default(),
        ));
        presentation
    });
    let request = LlmRequest {
        code_mode,
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
        let actual = fixture(api.clone(), case, false).await;
        assert_eq!(actual, entry["request"], "{api:?}: {case}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_builtin_inputs_avoid_top_level_combinators_with_and_without_code_mode() {
    for case in [
        "workspace",
        "environment",
        "canonical",
        "one_shot",
        "web",
        "workflow",
    ] {
        for code_mode in [false, true] {
            let wire = fixture(ProviderApiKind::AnthropicMessages, case, code_mode).await;
            let tools = wire["tools"].as_array().expect("advertised tools");
            assert!(tools.iter().any(|tool| tool["name"] == "blob_put"));
            for tool in tools {
                let Some(schema) = tool.get("input_schema") else {
                    continue; // Provider-native tools have no custom input schema.
                };
                assert_eq!(schema["type"], "object", "{case}: {}", tool["name"]);
                for keyword in ["oneOf", "anyOf", "allOf"] {
                    assert!(
                        schema.get(keyword).is_none(),
                        "{case}, code_mode={code_mode}: {} has top-level {keyword}",
                        tool["name"]
                    );
                }
                if matches!(
                    tool["name"].as_str(),
                    Some("blob_put" | "Write" | "VfsWrite" | "write_file" | "vfs_write_file")
                ) {
                    assert!(
                        tool["description"]
                            .as_str()
                            .unwrap()
                            .contains("exactly one")
                    );
                }
            }
        }
    }
}

/// Run explicitly after an intentional change to provider-visible tool definitions.
#[tokio::test(flavor = "current_thread")]
#[ignore = "regenerates the committed provider request fixture"]
async fn regenerate_builtin_provider_contracts() {
    let mut baseline: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/builtin_catalogs.json")).expect("baseline");
    for entry in &mut baseline {
        let api = serde_json::from_value(entry["api"].clone()).expect("API kind");
        entry["request"] = fixture(api, entry["case"].as_str().expect("case"), false).await;
    }
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/builtin_catalogs.json"
        ),
        format!("{}\n", serde_json::to_string_pretty(&baseline).unwrap()),
    )
    .unwrap();
}
