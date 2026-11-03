use super::*;
use tools::fs::{FileSystem, FsPath, FsToolContext, InMemoryFileSystem};
use tools::toolset::{BuiltinToolPresentation, EnvironmentToolsetConfig, ToolsetConfig};

fn editing_request(presentation: BuiltinToolPresentation) -> LlmGenerationRequest {
    let mut request = generation_request();
    let mut config = ToolsetConfig::workspace();
    config.builtin.environment = EnvironmentToolsetConfig::basic();
    config.builtin.presentation = presentation;
    request.request.tools = tools::toolset::register_toolset(&config)
        .unwrap()
        .tools
        .into_values()
        .collect();
    request
}

fn response(output: Value) -> ApiResponse<oai::Response> {
    let raw_json = json!({"id":"resp_patch", "status":"completed", "output":output});
    ApiResponse {
        parsed: serde_json::from_value(raw_json.clone()).unwrap(),
        raw_json,
        status: 200,
        headers: HeaderSnapshot::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn responses_patches_use_freeform_and_scripts_keep_json_arguments() {
    let blobs = InMemoryBlobStore::new();
    for presentation in [
        BuiltinToolPresentation::ProviderDefault,
        BuiltinToolPresentation::CodexLike,
    ] {
        for code_mode in [false, true] {
            let mut generation = editing_request(presentation);
            let request = &mut generation.request;
            if code_mode {
                request.code_mode = Some(harness::CodeModePresentation {
                    allowed_tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
                    ..Default::default()
                });
            }
            for (id, name) in [
                ("env.apply_patch", "apply_patch"),
                ("vfs.apply_patch", "vfs_apply_patch"),
            ] {
                request.tool_choice = Some(ToolChoice::Specific {
                    tool_name: ToolName::new(id),
                });
                let wire = serde_json::to_value(
                    materialize_create_request(&blobs, request).await.unwrap(),
                )
                .unwrap();
                assert_eq!(wire["tool_choice"], json!({"type":"custom","name":name}));
                let tools = wire["tools"].as_array().unwrap();
                assert!(!tools.iter().any(|tool| matches!(
                    tool["name"].as_str(),
                    Some("edit_file" | "vfs_edit_file")
                )));
                let patch = tools.iter().find(|tool| tool["name"] == name).unwrap();
                assert_eq!(patch["type"], "custom");
                assert_eq!(patch["format"]["type"], "grammar");
                assert_eq!(patch["format"]["syntax"], "lark");
                assert!(
                    patch["format"]["definition"]
                        .as_str()
                        .unwrap()
                        .contains("*** Begin Patch")
                );
                assert!(patch.get("parameters").is_none());
                let description = patch["description"].as_str().unwrap();
                assert!(description.contains("do not wrap the patch in JSON"));
                assert!(description.contains("Prefer this tool for ordinary text edits"));
                assert!(!description.contains("JSON `patch` argument"));
                if code_mode {
                    assert!(description.contains(&format!("await tools[\"{name}\"](args)")));
                    assert!(description.contains("object with a `patch` string"));
                    assert!(!description.contains("input schema above"));
                }
            }
            let callable = tools::callable::resolve(
                &blobs,
                &tools::runtime::ToolTarget::from(&request.model),
                &request.tools,
            )
            .await
            .unwrap()
            .into_iter()
            .find(|tool| tool.id.as_str() == "vfs.apply_patch")
            .unwrap()
            .into_callable()
            .unwrap();
            assert_eq!(
                callable.definition.input_schema["properties"]["patch"]["type"],
                "string"
            );
            assert_eq!(
                callable.definition.input_schema["required"],
                json!(["patch"])
            );
        }
    }
    let mut request = editing_request(BuiltinToolPresentation::Canonical).request;
    request.tool_choice = Some(ToolChoice::Specific {
        tool_name: ToolName::new("env.apply_patch"),
    });
    let wire =
        serde_json::to_value(materialize_create_request(&blobs, &request).await.unwrap()).unwrap();
    assert_eq!(
        wire["tool_choice"],
        json!({"type":"function","name":"apply_patch"})
    );
    let patch = wire["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "apply_patch")
        .unwrap();
    assert_eq!(patch["type"], "function");
    assert_eq!(patch["parameters"]["properties"]["patch"]["type"], "string");
}

#[tokio::test(flavor = "current_thread")]
async fn freeform_patches_normalize_for_execution_and_replay_original_call_types() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let patch =
        "*** Begin Patch\n*** Add File: /hello.txt\n+hello \"world\"\\test\n*** End Patch\n";
    let output = json!([
        {"type":"custom_tool_call","id":"ct_env","call_id":"env_patch","name":"apply_patch","input":patch,"extension":"retained"},
        {"type":"custom_tool_call","id":"ct_vfs","call_id":"vfs_patch","name":"vfs_apply_patch","input":patch},
        {"type":"function_call","id":"fc_old","call_id":"old_patch","name":"apply_patch","arguments":json!({"patch":patch}).to_string()}
    ]);
    let api = fake_api_with(response(output.clone()));
    let adapter = OpenAiResponsesLlmAdapter::new(api, blobs.clone());
    let execution = adapter
        .generate(editing_request(BuiltinToolPresentation::ProviderDefault))
        .await
        .unwrap();
    let result = execution.result;
    assert_eq!(result.status, LlmGenerationStatus::Succeeded);
    assert_eq!(result.facts.finish, LlmFinish::ToolCalls);
    assert_eq!(result.facts.tool_calls.len(), 3);
    for (call, id) in result.facts.tool_calls.iter().zip([
        "env.apply_patch",
        "vfs.apply_patch",
        "env.apply_patch",
    ]) {
        assert_eq!(call.tool_id, Some(ToolName::new(id)));
        assert_eq!(
            read_json(blobs.as_ref(), &call.arguments_ref)
                .await
                .unwrap(),
            json!({"patch":patch})
        );
        let fs = InMemoryFileSystem::full_access();
        let arguments = read_json(blobs.as_ref(), &call.arguments_ref)
            .await
            .unwrap();
        tools::fs::tools::invoke_apply_patch(
            &FsToolContext::new(Arc::new(fs.clone()), blobs.clone()),
            serde_json::from_value(arguments).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            fs.read_file_text(&FsPath::new("/hello.txt").unwrap())
                .await
                .unwrap(),
            "hello \"world\"\\test\n"
        );
    }
    let mut entries: Vec<_> = result
        .context_entries
        .iter()
        .enumerate()
        .map(|(index, entry)| retained_context_entry(index, entry))
        .collect();
    for call in &result.facts.tool_calls {
        let entry = ContextEntryInput {
            kind: ContextEntryKind::ToolResult {
                call_id: call.call_id.clone(),
                is_error: true,
            },
            content: harness::ContentRef::text(
                blobs
                    .insert_text("Patch failed: read-only filesystem")
                    .await,
            ),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        };
        entries.push(retained_context_entry(entries.len(), &entry));
    }
    // Switching presentation must not rewrite historical custom calls or
    // relabel a historical JSON call to the same exposed tool name.
    let mut followup = editing_request(BuiltinToolPresentation::Canonical).request;
    followup.context.entries = entries.clone();
    let wire = serde_json::to_value(
        materialize_create_request(blobs.as_ref(), &followup)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        &wire["input"].as_array().unwrap()[..3],
        output.as_array().unwrap()
    );
    for (index, kind) in [
        "custom_tool_call_output",
        "custom_tool_call_output",
        "function_call_output",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(wire["input"][index + 3]["type"], *kind);
        assert_eq!(
            wire["input"][index + 3]["call_id"],
            output[index]["call_id"]
        );
        assert_eq!(
            wire["input"][index + 3]["output"],
            "Patch failed: read-only filesystem"
        );
    }
    let task = ContextCompactionTask {
        model: followup.model,
        context: followup.context,
        request_fingerprint: "patch-compaction".into(),
        target_tokens: None,
        covered_entry_ids: Vec::new(),
        tools: Vec::new(),
        input_limit_tokens: None,
    };
    let compact = serde_json::to_value(
        materialize_compact_request(blobs.as_ref(), &task)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(compact["input"], wire["input"]);
}

#[tokio::test(flavor = "current_thread")]
async fn custom_calls_require_an_advertised_freeform_patch_and_string_input() {
    let blobs = InMemoryBlobStore::new();
    for (presentation, name, input) in [
        (
            BuiltinToolPresentation::Canonical,
            "apply_patch",
            json!("patch"),
        ),
        (
            BuiltinToolPresentation::ProviderDefault,
            "read_file",
            json!("patch"),
        ),
        (
            BuiltinToolPresentation::ProviderDefault,
            "unknown_patch",
            json!("patch"),
        ),
        (
            BuiltinToolPresentation::ProviderDefault,
            "apply_patch",
            Value::Null,
        ),
    ] {
        let request = editing_request(presentation);
        let response = response(
            json!([{"type":"custom_tool_call","call_id":"call","name":name,"input":input}]),
        );
        assert!(matches!(
            result_from_response(&blobs, &request, &response).await,
            Err(LlmAdapterError::InvalidProviderResponse { .. })
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_freeform_patches_never_execute_or_enter_replay() {
    let blobs = InMemoryBlobStore::new();
    let request = editing_request(BuiltinToolPresentation::ProviderDefault);
    let mut response = response(json!([{
        "type":"custom_tool_call", "call_id":"partial", "name":"apply_patch",
        "input":"*** Begin Patch\n*** Add File: incomplete.txt\n+partial"
    }]));
    response.raw_json["status"] = json!("incomplete");
    response.raw_json["incomplete_details"] = json!({"reason":"max_output_tokens"});
    response.parsed = serde_json::from_value(response.raw_json.clone()).unwrap();
    let result = result_from_response(&blobs, &request, &response)
        .await
        .unwrap();
    assert_eq!(result.status, LlmGenerationStatus::Failed);
    assert_eq!(result.facts.finish, LlmFinish::Length);
    assert!(result.facts.tool_calls.is_empty());
    assert!(
        result
            .context_entries
            .iter()
            .all(|entry| !matches!(entry.kind, ContextEntryKind::ToolCall { .. }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reused_call_ids_do_not_carry_freeform_output_types_into_later_turns() {
    let blobs = InMemoryBlobStore::new();
    let mut entries = Vec::new();
    for kind in ["custom_tool_call", "function_call"] {
        let native = json!({"type":kind,"call_id":"reused","name":"apply_patch"});
        let call = ContextEntryInput {
            kind: ContextEntryKind::ToolCall {
                call_id: ToolCallId::new("reused"),
                name: ToolName::new("apply_patch"),
            },
            content: harness::ContentRef {
                content_ref: put_json(&blobs, &native).await.unwrap(),
                media_type: Some(MEDIA_TYPE_JSON.into()),
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        };
        entries.push(retained_context_entry(entries.len(), &call));
        let result = ContextEntryInput {
            kind: ContextEntryKind::ToolResult {
                call_id: ToolCallId::new("reused"),
                is_error: false,
            },
            content: harness::ContentRef::text(blobs.insert_text("done").await),
            ..call
        };
        entries.push(retained_context_entry(entries.len(), &result));
    }
    let input =
        serde_json::to_value(materialize_input_items(&blobs, &entries).await.unwrap()).unwrap();
    assert_eq!(input[1]["type"], "custom_tool_call_output");
    assert_eq!(input[3]["type"], "function_call_output");
}
