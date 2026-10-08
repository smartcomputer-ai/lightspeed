use harness::{
    CodeModePresentation, ContextSnapshot, FunctionToolSpec, LlmRequest, ModelSelection,
    ProviderApiKind, ToolKind, ToolName, ToolSpec, WorkflowToolCompletion,
    WorkflowToolCompletionKeySource, WorkflowToolResultContract,
    storage::{BlobStore, InMemoryBlobStore},
};
use serde_json::{Value, json};

fn builtin(id: &str) -> ToolSpec {
    tools::definitions::register(
        id,
        Default::default(),
        harness::ToolParallelism::ParallelSafe,
        Default::default(),
    )
}

async fn render(blobs: &InMemoryBlobStore, request: &LlmRequest) -> Value {
    match request.model.api_kind {
        ProviderApiKind::OpenAiResponses => serde_json::to_value(
            llm_runtime::openai_responses::materialize_create_request(blobs, request)
                .await
                .unwrap(),
        ),
        ProviderApiKind::OpenAiCompletions => serde_json::to_value(
            llm_runtime::openai_completions::materialize_create_request(blobs, request)
                .await
                .unwrap(),
        ),
        ProviderApiKind::AnthropicMessages => serde_json::to_value(
            llm_runtime::anthropic_messages::materialize_create_request(blobs, request)
                .await
                .unwrap(),
        ),
    }
    .unwrap()
}

fn functions(wire: &Value) -> Vec<&Value> {
    wire["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool.get("function").unwrap_or(tool))
        .collect()
}

fn function<'a>(wire: &'a Value, name: &str) -> &'a Value {
    functions(wire)
        .into_iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| {
            panic!(
                "missing {name}; advertised names: {:?}",
                functions(wire)
                    .iter()
                    .map(|tool| &tool["name"])
                    .collect::<Vec<_>>()
            )
        })
}

fn return_schema(tool: &Value) -> Value {
    serde_json::from_str(
        tool["description"]
            .as_str()
            .unwrap()
            .split_once("\nReturn JSON Schema: ")
            .expect("rendered return contract")
            .1,
    )
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn descriptions_match_call_results_without_changing_provider_input_contracts() {
    let blobs = InMemoryBlobStore::new();
    let input_ref = blobs
        .put_bytes(br#"{"type":"object","properties":{}}"#.to_vec())
        .await
        .unwrap();
    let declared = json!({"type":"array","items":{"type":"string"}});
    let declared_ref = blobs
        .put_bytes(serde_json::to_vec(&declared).unwrap())
        .await
        .unwrap();
    let mut authored = ToolSpec {
        name: ToolName::new("authored"),
        kind: ToolKind::Function(FunctionToolSpec {
            description_ref: Some(
                blobs
                    .put_bytes(b"Keep this usage guidance.".to_vec())
                    .await
                    .unwrap(),
            ),
            input_schema_ref: input_ref,
            output_schema_ref: Some(declared_ref.clone()),
            strict: Some(false),
            provider_options_ref: None,
        }),
        execution: Default::default(),
        parallelism: harness::ToolParallelism::ParallelSafe,
    };
    let mut unknown = authored.clone();
    unknown.name = ToolName::new("unknown");
    if let ToolKind::Function(spec) = &mut unknown.kind {
        spec.output_schema_ref = None;
    }
    let mut never = unknown.clone();
    never.name = ToolName::new("never");
    if let ToolKind::Function(spec) = &mut never.kind {
        spec.output_schema_ref = Some(blobs.put_bytes(b"false".to_vec()).await.unwrap());
    }
    // A joined workflow's reply overrides an authored substrate declaration.
    if let ToolKind::Function(spec) = &mut authored.kind {
        spec.output_schema_ref = Some(blobs.put_bytes(b"true".to_vec()).await.unwrap());
    }
    for api_kind in [
        ProviderApiKind::OpenAiResponses,
        ProviderApiKind::OpenAiCompletions,
        ProviderApiKind::AnthropicMessages,
    ] {
        let tools = vec![
            builtin("vfs.read_file"),
            builtin("concurrency.sleep"),
            builtin("concurrency.await"),
            builtin("subagent.spawn"),
            builtin("env.job_run"),
            builtin("code.execute"),
            authored.clone(),
            unknown.clone(),
            never.clone(),
        ];
        let allowed_tools = tools
            .iter()
            .filter(|tool| tool.name.as_str() != "unknown")
            .map(|tool| tool.name.clone())
            .collect();
        let mut request = LlmRequest {
            model: ModelSelection {
                api_kind: api_kind.clone(),
                provider_id: "provider".into(),
                model: if api_kind == ProviderApiKind::AnthropicMessages {
                    "claude-opus-4-8"
                } else {
                    "gpt-5.1"
                }
                .into(),
            },
            request_fingerprint: "code-mode-schema-test".into(),
            context: ContextSnapshot {
                api_kind,
                context_revision: 0,
                entries: vec![],
                token_estimate: None,
            },
            tools,
            code_mode: None,
            tool_choice: None,
            output_limit: Some(4096),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            provider_response_id: None,
            compaction: None,
            params: None,
        };
        let ordinary = render(&blobs, &request).await;
        request.code_mode = Some(CodeModePresentation {
            allowed_tools,
            workflow_results: [
                (
                    ToolName::new("authored"),
                    WorkflowToolResultContract {
                        starts_workflow: true,
                        completion: WorkflowToolCompletion::Joined {
                            reply_schema_ref: Some(declared_ref.clone()),
                            deadline_after_ms: 1000,
                        },
                    },
                ),
                (
                    ToolName::new("env.job_run"),
                    WorkflowToolResultContract {
                        starts_workflow: true,
                        completion: WorkflowToolCompletion::Joined {
                            reply_schema_ref: None,
                            deadline_after_ms: 1000,
                        },
                    },
                ),
                (
                    ToolName::new("subagent.spawn"),
                    WorkflowToolResultContract {
                        starts_workflow: true,
                        completion: WorkflowToolCompletion::Promises {
                            reply_schema_ref: Some(declared_ref.clone()),
                            deadline_after_ms: None,
                            max_promises: 1,
                            key_source: WorkflowToolCompletionKeySource::Reply,
                        },
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        let presented = render(&blobs, &request).await;
        for (before, after) in functions(&ordinary).into_iter().zip(functions(&presented)) {
            let mut restored = after.clone();
            if let Some(description) = before.get("description") {
                restored["description"] = description.clone();
            } else {
                restored.as_object_mut().unwrap().remove("description");
            }
            assert_eq!(&restored, before, "only descriptions change");
        }
        let read_name = if request.model.api_kind == ProviderApiKind::AnthropicMessages {
            "VfsRead"
        } else {
            "vfs_read_file"
        };
        let read = function(&presented, read_name);
        assert!(
            read["description"]
                .as_str()
                .unwrap()
                .contains(&format!("await tools[\"{read_name}\"](args)"))
        );
        assert_eq!(return_schema(function(&presented, "authored")), declared);
        assert!(
            function(&presented, "authored")["description"]
                .as_str()
                .unwrap()
                .starts_with("Keep this usage guidance.")
        );
        assert_eq!(return_schema(function(&presented, "never")), json!(false));
        let unknown_description = function(&presented, "unknown")["description"]
            .as_str()
            .unwrap();
        assert!(unknown_description.contains("not available inside"));
        assert!(unknown_description.contains("No output schema is declared"));
        assert!(
            function(&presented, "job_run")["description"]
                .as_str()
                .unwrap()
                .contains("No output schema is declared"),
            "an arbitrary workflow must not inherit the job substrate schema"
        );
        let acknowledgement = return_schema(function(&presented, "agent_spawn"));
        assert_eq!(acknowledgement["properties"]["accepted"]["const"], true);
        assert!(
            acknowledgement["required"]
                .as_array()
                .unwrap()
                .contains(&json!("promise"))
        );
        assert!(
            acknowledgement["required"]
                .as_array()
                .unwrap()
                .contains(&json!("executionId"))
        );
        let wait = return_schema(function(&presented, "await"));
        assert!(wait["properties"].get("results").is_some());
        assert!(
            return_schema(function(&presented, "sleep"))["properties"]
                .get("promise")
                .is_some()
        );
        assert!(
            function(&presented, "code_execute")["description"]
                .as_str()
                .unwrap()
                .contains("recursive script calls are unavailable")
        );
        request.code_mode = None;
        assert_eq!(
            render(&blobs, &request).await,
            ordinary,
            "disabled feature preserves descriptions"
        );
    }
}
