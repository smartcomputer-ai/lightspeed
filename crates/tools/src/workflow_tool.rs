//! Runtime adapter for workflow-backed function tools.

use std::collections::BTreeMap;

use engine::{
    BlobRef, PromiseId, PromiseIdAllocator, REPLY_COMPLETION_KEY, RunId, SessionId, ToolBatchId,
    ToolInvocationRequest, ToolKind, TurnId, WorkflowToolBinding, WorkflowToolCompletion,
    WorkflowToolCompletionKeySource, WorkflowToolDefinition, WorkflowToolInvocation,
    WorkflowToolInvocationId, WorkflowToolTarget, storage::BlobStore, validate_completion_key,
    with_completion_deadline, workflow_tool_emit_effect, workflow_tool_execution_id,
};
use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::ToolInvocationOutput,
};

/// Validate every CAS document needed to present and invoke a workflow tool.
pub async fn validate_workflow_tool_definition_documents(
    blobs: &dyn BlobStore,
    definition: &WorkflowToolDefinition,
) -> ToolResult<()> {
    if let ToolKind::Builtin(spec) = &definition.tool.kind {
        let target =
            crate::runtime::ToolTarget::api_kind(engine::ProviderApiKind::OpenAiCompletions);
        let resolved = crate::definitions::resolve(&definition.tool.name, spec, &target)?;
        if resolved.is_empty()
            || resolved
                .iter()
                .any(|tool| !matches!(tool.definition, crate::definitions::Definition::Function(_)))
        {
            return Err(ToolError::InvalidRequest {
                message: "workflow tools require a client function definition".to_owned(),
            });
        }
        for tool in resolved {
            if let crate::definitions::Definition::Function(function) = tool.definition {
                jsonschema::validator_for(&function.input_schema).map_err(|error| {
                    ToolError::InvalidRequest {
                        message: format!("invalid built-in input schema: {error}"),
                    }
                })?;
            }
        }
        return Ok(());
    }
    let ToolKind::Function(function) = &definition.tool.kind else {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "workflow tool {} is not a function tool",
                definition.tool_id
            ),
        });
    };

    if let Some(description_ref) = &function.description_ref {
        blobs
            .read_text(description_ref)
            .await
            .map_err(ToolError::from)?;
    }
    compile_schema(blobs, &function.input_schema_ref, "input").await?;
    if let Some(output_schema_ref) = &function.output_schema_ref {
        compile_schema(blobs, output_schema_ref, "output").await?;
    }
    if let Some(provider_options_ref) = &function.provider_options_ref {
        read_json(blobs, provider_options_ref, "provider options").await?;
    }
    Ok(())
}

/// Validate an optional Promise-resolution schema when a workflow-tool
/// binding requires typed replies.
pub async fn validate_workflow_tool_reply_schema(
    blobs: &dyn BlobStore,
    schema_ref: &BlobRef,
) -> ToolResult<()> {
    compile_schema(blobs, schema_ref, "reply").await
}

/// Validate one CAS-backed argument object against its admitted input schema.
pub async fn validate_workflow_tool_arguments(
    blobs: &dyn BlobStore,
    binding: &WorkflowToolBinding,
    call: &ToolInvocationRequest,
) -> ToolResult<Value> {
    let arguments = read_json(blobs, &call.arguments_ref, "arguments").await?;
    let schema = match &binding.definition.tool.kind {
        ToolKind::Function(function) => {
            read_json(blobs, &function.input_schema_ref, "input schema").await?
        }
        ToolKind::Builtin(spec) => {
            let runtime = call
                .builtin
                .as_ref()
                .ok_or_else(|| ToolError::InvalidRequest {
                    message: "built-in workflow call is missing original turn inputs".to_owned(),
                })?;
            crate::definitions::resolve(
                &binding.definition.tool.name,
                spec,
                &crate::runtime::ToolTarget::from(&runtime.model),
            )?
            .into_iter()
            .find(|tool| tool.name == call.tool_name)
            .and_then(|tool| match tool.definition {
                crate::definitions::Definition::Function(function) => Some(function.input_schema),
                _ => None,
            })
            .ok_or_else(|| ToolError::InvalidRequest {
                message: "workflow call has no matching function presentation".to_owned(),
            })?
        }
        _ => {
            return Err(ToolError::InvalidRequest {
                message: "workflow tool requires a function definition".to_owned(),
            });
        }
    };
    let validator =
        jsonschema::validator_for(&schema).map_err(|error| ToolError::InvalidRequest {
            message: format!(
                "workflow tool {} has an unsupported input schema: {error}",
                binding.definition.tool_id
            ),
        })?;
    if let Err(error) = validator.validate(&arguments) {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "arguments for workflow tool {} do not match its input schema: {error}",
                binding.definition.tool_id
            ),
        });
    }
    Ok(arguments)
}

/// Build the stable acknowledgement and trusted internal effect for a valid
/// workflow-tool call. The caller persists the ordinary tool output.
///
/// `now_ms` is the runtime's observed wall clock, used only to convert the
/// binding's trusted relative deadline into an absolute per-promise
/// deadline; the durable invocation record itself carries no time.
///
/// Completion promises are numbered from the batch's allocator; the model
/// sees only the promise handle(s) it can act on, while `output_json`
/// keeps the invocation and execution ids for clients.
#[allow(clippy::too_many_arguments)]
pub async fn invoke_workflow_tool(
    blobs: &dyn BlobStore,
    binding: &WorkflowToolBinding,
    session_id: &SessionId,
    run_id: RunId,
    turn_id: TurnId,
    tool_batch_id: ToolBatchId,
    call: &ToolInvocationRequest,
    execution_context_ref: Option<BlobRef>,
    promise_ids: &PromiseIdAllocator,
    now_ms: u64,
) -> ToolResult<ToolInvocationOutput> {
    binding
        .validate()
        .map_err(|error| ToolError::InvalidRequest {
            message: format!(
                "invalid workflow tool {} binding: {error}",
                binding.definition.tool_id
            ),
        })?;
    if call.tool_id.as_ref() != Some(&binding.definition.tool.name) {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "workflow tool {} is bound to tool {}, got {}",
                binding.definition.tool_id, binding.definition.tool.name, call.tool_name
            ),
        });
    }
    let arguments = validate_workflow_tool_arguments(blobs, binding, call).await?;

    let invocation_id = WorkflowToolInvocationId::for_call(
        binding.session_universe_id,
        session_id,
        run_id,
        turn_id,
        tool_batch_id,
        &call.call_id,
        &binding.binding_fingerprint,
    );
    let (completion_promises, completion_deadline_ms) = match &binding.completion {
        WorkflowToolCompletion::Accepted => (None, None),
        WorkflowToolCompletion::Joined {
            deadline_after_ms, ..
        } => {
            let promises = BTreeMap::from([(
                engine::REPLY_COMPLETION_KEY.to_owned(),
                promise_ids.allocate(),
            )]);
            (
                Some(promises),
                Some(now_ms.saturating_add(*deadline_after_ms)),
            )
        }
        WorkflowToolCompletion::Promises {
            deadline_after_ms, ..
        } => {
            let keys = derive_completion_keys(binding, &arguments)?;
            let promises: BTreeMap<String, PromiseId> = keys
                .into_iter()
                .map(|key| (key, promise_ids.allocate()))
                .collect();
            (
                Some(promises),
                deadline_after_ms.map(|after| now_ms.saturating_add(after)),
            )
        }
    };
    let invocation = WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: binding.definition.tool_id.clone(),
        semantic_type: binding.definition.semantic_type.clone(),
        schema_revision: binding.definition.revision,
        binding_fingerprint: binding.binding_fingerprint.clone(),
        session_universe_id: binding.session_universe_id,
        session_id: session_id.clone(),
        run_id,
        turn_id,
        tool_batch_id,
        tool_call_id: call.call_id.clone(),
        arguments_ref: call.arguments_ref.clone(),
        execution_context_ref,
        completion_promises,
    };
    let mut acknowledgement = json!({
        "accepted": true,
        "invocationId": invocation_id.as_str(),
    });
    if let WorkflowToolTarget::Start { start } = &binding.target {
        acknowledgement["executionId"] = Value::String(workflow_tool_execution_id(
            &invocation_id,
            &start.recipe_fingerprint,
        ));
    }
    // The model gets exactly what it can act on: the single promise of a
    // reply-keyed call, or the keyed map of a multi-item call. Invocation
    // and execution ids are client diagnostics and stay in `output_json`.
    let mut model_visible = json!({ "accepted": true });
    if let WorkflowToolCompletion::Promises { key_source, .. } = &binding.completion
        && let Some(promises) = &invocation.completion_promises
    {
        if matches!(key_source, WorkflowToolCompletionKeySource::Reply)
            && let Some(promise_id) = promises.get(REPLY_COMPLETION_KEY)
        {
            let promise = Value::String(promise_id.to_string());
            acknowledgement["promise"] = promise.clone();
            model_visible["promise"] = promise;
        } else {
            let map: serde_json::Map<String, Value> = promises
                .iter()
                .map(|(key, promise_id)| (key.clone(), Value::String(promise_id.to_string())))
                .collect();
            acknowledgement["promises"] = Value::Object(map.clone());
            model_visible["promises"] = Value::Object(map);
        }
    }
    let effect = with_completion_deadline(
        workflow_tool_emit_effect(&invocation),
        completion_deadline_ms,
    );
    Ok(ToolInvocationOutput {
        model_visible_text: model_visible.to_string(),
        output_json: acknowledgement,
        effects: vec![effect],
        media: Vec::new(),
    })
}

/// Completion keys come only from the binding's declarative contract over
/// the schema-validated arguments: absent key pointer means the single
/// reserved `reply` key; a key pointer names an array of unique key strings
/// (one promise per validated work item). The model influences the key
/// count only through its validated arguments; it never names promise ids,
/// receivers, or transports.
fn derive_completion_keys(
    binding: &WorkflowToolBinding,
    arguments: &Value,
) -> ToolResult<Vec<String>> {
    let (max_promises, key_source) = match &binding.completion {
        WorkflowToolCompletion::Promises {
            max_promises,
            key_source,
            ..
        } => (*max_promises, key_source),
        WorkflowToolCompletion::Joined { .. } => {
            return Ok(vec![engine::REPLY_COMPLETION_KEY.to_owned()]);
        }
        WorkflowToolCompletion::Accepted => return Ok(Vec::new()),
    };
    derive_completion_keys_from_source(
        &binding.definition.tool_id,
        max_promises,
        key_source,
        arguments,
    )
}

fn derive_completion_keys_from_source(
    tool_id: &engine::WorkflowToolId,
    max_promises: u32,
    source: &engine::WorkflowToolCompletionKeySource,
    arguments: &Value,
) -> ToolResult<Vec<String>> {
    match source {
        engine::WorkflowToolCompletionKeySource::Reply => Ok(vec![REPLY_COMPLETION_KEY.to_owned()]),
        engine::WorkflowToolCompletionKeySource::StringArray { pointer } => {
            let Some(Value::Array(entries)) = arguments.pointer(pointer) else {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} arguments do not contain a key array at {pointer}"
                    ),
                });
            };
            if entries.is_empty() || entries.len() as u32 > max_promises {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} requires 1..={max_promises} completion keys at {pointer}, got {}",
                        entries.len()
                    ),
                });
            }
            let mut keys = Vec::with_capacity(entries.len());
            for entry in entries {
                let Value::String(key) = entry else {
                    return Err(ToolError::InvalidRequest {
                        message: format!(
                            "workflow tool {tool_id} completion keys at {pointer} must be strings"
                        ),
                    });
                };
                validate_completion_key(key).map_err(|error| ToolError::InvalidRequest {
                    message: format!("workflow tool {tool_id} completion key is invalid: {error}"),
                })?;
                if keys.contains(key) {
                    return Err(ToolError::InvalidRequest {
                        message: format!(
                            "workflow tool {tool_id} completion keys at {pointer} must be unique"
                        ),
                    });
                }
                keys.push(key.clone());
            }
            Ok(keys)
        }
        engine::WorkflowToolCompletionKeySource::ArrayItemField { pointer, field } => {
            let Some(Value::Array(entries)) = arguments.pointer(pointer) else {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} arguments do not contain an item array at {pointer}"
                    ),
                });
            };
            if entries.is_empty() || entries.len() as u32 > max_promises {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} requires 1..={max_promises} completion items at {pointer}, got {}",
                        entries.len()
                    ),
                });
            }
            let mut keys = Vec::with_capacity(entries.len());
            for (index, entry) in entries.iter().enumerate() {
                let Some(Value::String(key)) = entry.get(field) else {
                    return Err(ToolError::InvalidRequest {
                        message: format!(
                            "workflow tool {tool_id} item {pointer}/{index} needs a string `{field}`"
                        ),
                    });
                };
                validate_completion_key(key).map_err(|error| ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} item {pointer}/{index} has an invalid `{field}`: {error}"
                    ),
                })?;
                if keys.contains(key) {
                    return Err(ToolError::InvalidRequest {
                        message: format!(
                            "workflow tool {tool_id} items at {pointer} must have unique `{field}` values, got {key:?} twice"
                        ),
                    });
                }
                keys.push(key.clone());
            }
            Ok(keys)
        }
        engine::WorkflowToolCompletionKeySource::ArrayIndices { pointer, prefix } => {
            let Some(Value::Array(entries)) = arguments.pointer(pointer) else {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} arguments do not contain an item array at {pointer}"
                    ),
                });
            };
            if entries.is_empty() || entries.len() as u32 > max_promises {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "workflow tool {tool_id} requires 1..={max_promises} completion items at {pointer}, got {}",
                        entries.len()
                    ),
                });
            }
            Ok((0..entries.len())
                .map(|index| format!("{prefix}{index}"))
                .collect())
        }
    }
}

async fn compile_schema(blobs: &dyn BlobStore, schema_ref: &BlobRef, kind: &str) -> ToolResult<()> {
    let schema = read_json(blobs, schema_ref, &format!("{kind} schema")).await?;
    jsonschema::validator_for(&schema).map_err(|error| ToolError::InvalidRequest {
        message: format!("workflow tool {kind} schema is unsupported: {error}"),
    })?;
    Ok(())
}

async fn read_json(blobs: &dyn BlobStore, blob_ref: &BlobRef, kind: &str) -> ToolResult<Value> {
    let bytes = blobs.read_bytes(blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| ToolError::InvalidRequest {
        message: format!("workflow tool {kind} is not valid JSON: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        FunctionToolSpec, ToolCallId, ToolKind, ToolName, ToolParallelism, ToolSpec,
        WorkflowEndpointRef, WorkflowToolId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::toolset::{ToolsetConfig, register_toolset, register_workflow_tools};

    async fn binding(blobs: &dyn BlobStore) -> WorkflowToolBinding {
        let schema_ref = blobs
            .put_bytes(
                serde_json::to_vec(&json!({
                    "type": "object",
                    "properties": { "status": { "type": "string" } },
                    "required": ["status"],
                    "additionalProperties": false
                }))
                .expect("schema"),
            )
            .await
            .expect("put schema");
        WorkflowToolBinding::admit_bound_notify(
            Uuid::from_u128(1),
            WorkflowToolDefinition {
                tool_id: WorkflowToolId::new("report"),
                revision: 1,
                semantic_type: "lightspeed.work.report.v1".to_owned(),
                tool: ToolSpec {
                    name: ToolName::new("work_report"),
                    kind: ToolKind::Function(FunctionToolSpec {
                        description_ref: None,
                        input_schema_ref: schema_ref,
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: ToolParallelism::ParallelSafe,
                    execution: Default::default(),
                },
            },
            WorkflowEndpointRef {
                workflow_id: "work arbitrary id".to_owned(),
                workflow_kind: "agent_work".to_owned(),
            },
        )
        .expect("binding")
    }

    #[tokio::test]
    async fn valid_call_returns_stable_ack_and_typed_effect() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = binding(blobs.as_ref()).await;
        let arguments_ref = blobs
            .put_bytes(br#"{"status":"complete"}"#.to_vec())
            .await
            .expect("arguments");
        let call = ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("work_report")),
            tool_name: ToolName::new("work_report"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };
        let first = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke");
        let retry = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("retry");

        assert_eq!(first, retry);
        assert_eq!(first.effects.len(), 1);
        assert_eq!(
            first.effects[0].kind,
            engine::WORKFLOW_TOOL_EMIT_EFFECT_KIND
        );
    }

    async fn promise_bearing_binding(
        blobs: &dyn BlobStore,
        max_promises: u32,
        key_source: engine::WorkflowToolCompletionKeySource,
    ) -> WorkflowToolBinding {
        let schema_ref = blobs
            .put_bytes(serde_json::to_vec(&json!({ "type": "object" })).expect("schema"))
            .await
            .expect("put schema");
        WorkflowToolBinding::admit(
            Uuid::from_u128(1),
            WorkflowToolDefinition {
                tool_id: WorkflowToolId::new("approve"),
                revision: 1,
                semantic_type: "lightspeed.approval.request.v1".to_owned(),
                tool: ToolSpec {
                    name: ToolName::new("request_approval"),
                    kind: ToolKind::Function(FunctionToolSpec {
                        description_ref: None,
                        input_schema_ref: schema_ref,
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: ToolParallelism::ParallelSafe,
                    execution: Default::default(),
                },
            },
            WorkflowToolTarget::Bound {
                receiver: WorkflowEndpointRef {
                    workflow_id: "approval plugin id".to_owned(),
                    workflow_kind: "approvals".to_owned(),
                },
                dispatch: engine::BoundWorkflowToolDispatch::Push,
            },
            WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: Some(30_000),
                max_promises,
                key_source,
            },
        )
        .expect("promise-bearing binding")
    }

    #[tokio::test]
    async fn promise_bearing_call_returns_keyed_promise_map_and_deadline() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = promise_bearing_binding(
            blobs.as_ref(),
            1,
            engine::WorkflowToolCompletionKeySource::Reply,
        )
        .await;
        let arguments_ref = blobs
            .put_bytes(br#"{"question":"deploy?"}"#.to_vec())
            .await
            .expect("arguments");
        let call = ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("request_approval")),
            tool_name: ToolName::new("request_approval"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };
        let output = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke");

        assert_eq!(output.output_json["promise"], json!("promise_1"));
        assert_eq!(
            output.model_visible_text, r#"{"accepted":true,"promise":"promise_1"}"#,
            "the model sees the promise handle and nothing else"
        );
        assert!(output.output_json["invocationId"].is_string());
        assert_eq!(output.effects.len(), 1);
        assert_eq!(
            output.effects[0].data.get("completion_deadline_ms"),
            Some(&"31000".to_owned())
        );
    }

    #[tokio::test]
    async fn joined_call_keeps_internal_reply_out_of_model_acknowledgement() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let explicit = promise_bearing_binding(
            blobs.as_ref(),
            1,
            engine::WorkflowToolCompletionKeySource::Reply,
        )
        .await;
        let binding = WorkflowToolBinding::admit(
            explicit.session_universe_id,
            explicit.definition,
            explicit.target,
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            },
        )
        .expect("Joined binding");
        let mut toolset_config = crate::toolset::ToolsetConfig::empty();
        crate::toolset::enable_concurrency_for_workflow_tools(&mut toolset_config, [&binding]);
        assert!(!toolset_config.concurrency.enabled);
        let arguments_ref = blobs
            .put_bytes(br#"{"question":"deploy?"}"#.to_vec())
            .await
            .expect("arguments");
        let call = ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("request_approval")),
            tool_name: ToolName::new("request_approval"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };

        let output = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke Joined call");

        assert!(output.output_json.get("promises").is_none());
        assert_eq!(output.model_visible_text, r#"{"accepted":true}"#);
        assert_eq!(
            output.effects[0].data.get("completion_deadline_ms"),
            Some(&"31000".to_owned())
        );
        let encoded_promises = output.effects[0]
            .data
            .get("completion_promises")
            .expect("internal reply map");
        assert!(encoded_promises.contains(engine::REPLY_COMPLETION_KEY));
        assert!(encoded_promises.contains("promise_1"));
    }

    #[tokio::test]
    async fn string_array_source_derives_one_promise_per_validated_work_item() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = promise_bearing_binding(
            blobs.as_ref(),
            4,
            engine::WorkflowToolCompletionKeySource::StringArray {
                pointer: "/jobs".to_owned(),
            },
        )
        .await;
        let call = |arguments_ref| ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("request_approval")),
            tool_name: ToolName::new("request_approval"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };

        let arguments_ref = blobs
            .put_bytes(br#"{"jobs":["build","test"]}"#.to_vec())
            .await
            .expect("arguments");
        let output = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call(arguments_ref),
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke multi-key");
        let promises = output.output_json["promises"]
            .as_object()
            .expect("promise map");
        assert_eq!(promises.len(), 2);
        assert!(promises.contains_key("build") && promises.contains_key("test"));

        // Duplicate keys are an ordinary failed call, not an emission.
        let duplicate_ref = blobs
            .put_bytes(br#"{"jobs":["build","build"]}"#.to_vec())
            .await
            .expect("arguments");
        let error = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call(duplicate_ref),
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect_err("duplicate keys");
        assert!(matches!(error, ToolError::InvalidRequest { .. }));

        // Exceeding the binding cap is an ordinary failed call.
        let over_cap_ref = blobs
            .put_bytes(br#"{"jobs":["a","b","c","d","e"]}"#.to_vec())
            .await
            .expect("arguments");
        let error = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call(over_cap_ref),
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect_err("cap exceeded");
        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    #[tokio::test]
    async fn array_indices_source_derives_keys_for_object_items() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = promise_bearing_binding(
            blobs.as_ref(),
            4,
            engine::WorkflowToolCompletionKeySource::ArrayIndices {
                pointer: "/jobs".to_owned(),
                prefix: "job-".to_owned(),
            },
        )
        .await;
        let arguments_ref = blobs
            .put_bytes(br#"{"jobs":[{"argv":["build"]},{"argv":["test"]}]}"#.to_vec())
            .await
            .expect("arguments");
        let call = ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("request_approval")),
            tool_name: ToolName::new("request_approval"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };

        let output = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke object items");

        let promises = output.output_json["promises"]
            .as_object()
            .expect("promise map");
        assert_eq!(promises.len(), 2);
        assert!(promises.contains_key("job-0"));
        assert!(promises.contains_key("job-1"));
    }

    #[tokio::test]
    async fn array_item_field_source_keys_promises_by_the_model_s_item_names() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = promise_bearing_binding(
            blobs.as_ref(),
            4,
            engine::WorkflowToolCompletionKeySource::ArrayItemField {
                pointer: "/jobs".to_owned(),
                field: "job_id".to_owned(),
            },
        )
        .await;
        let call = |arguments_ref| ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("request_approval")),
            tool_name: ToolName::new("request_approval"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };
        let invoke = |arguments: &'static [u8]| {
            let blobs = blobs.clone();
            let binding = binding.clone();
            async move {
                let arguments_ref = blobs
                    .put_bytes(arguments.to_vec())
                    .await
                    .expect("arguments");
                invoke_workflow_tool(
                    blobs.as_ref(),
                    &binding,
                    &SessionId::new("session-1"),
                    RunId::new(1),
                    TurnId::new(2),
                    ToolBatchId::new(3),
                    &call(arguments_ref),
                    None,
                    &PromiseIdAllocator::new(7),
                    1_000,
                )
                .await
            }
        };

        let output = invoke(br#"{"jobs":[{"job_id":"build","argv":["make"]},{"job_id":"test","argv":["make","test"]}]}"#)
            .await
            .expect("invoke keyed by job id");
        assert_eq!(
            output.output_json["promises"],
            json!({ "build": "promise_7", "test": "promise_8" })
        );
        assert_eq!(
            output.model_visible_text,
            r#"{"accepted":true,"promises":{"build":"promise_7","test":"promise_8"}}"#
        );

        for arguments in [
            br#"{"jobs":[{"argv":["make"]}]}"#.as_slice(),
            br#"{"jobs":[{"job_id":7}]}"#.as_slice(),
            br#"{"jobs":[{"job_id":"build"},{"job_id":"build"}]}"#.as_slice(),
            br#"{"jobs":[{"job_id":"build:1"}]}"#.as_slice(),
            br#"{"jobs":[]}"#.as_slice(),
        ] {
            let error = invoke(arguments).await.expect_err("rejected item set");
            assert!(matches!(error, ToolError::InvalidRequest { .. }));
        }
    }

    #[tokio::test]
    async fn start_target_call_acknowledges_execution_and_keyed_promises() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let schema_ref = blobs
            .put_bytes(serde_json::to_vec(&json!({ "type": "object" })).expect("schema"))
            .await
            .expect("put schema");
        let binding = WorkflowToolBinding::admit(
            Uuid::from_u128(1),
            WorkflowToolDefinition {
                tool_id: WorkflowToolId::new("launch"),
                revision: 1,
                semantic_type: "lightspeed.job.launch.v1".to_owned(),
                tool: ToolSpec {
                    name: ToolName::new("launch_job"),
                    kind: ToolKind::Function(FunctionToolSpec {
                        description_ref: None,
                        input_schema_ref: schema_ref,
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: ToolParallelism::ParallelSafe,
                    execution: Default::default(),
                },
            },
            WorkflowToolTarget::Start {
                start: engine::WorkflowStartRef {
                    recipe_format: 1,
                    revision: 1,
                    recipe_ref: BlobRef::from_bytes(b"recipe"),
                    recipe_fingerprint: "wtr:sha256:recipe".to_owned(),
                },
            },
            WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: None,
                max_promises: 1,
                key_source: engine::WorkflowToolCompletionKeySource::Reply,
            },
        )
        .expect("start binding");
        let arguments_ref = blobs
            .put_bytes(br#"{"job":"build"}"#.to_vec())
            .await
            .expect("arguments");
        let call = ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new("call-1"),
            tool_id: Some(ToolName::new("launch_job")),
            tool_name: ToolName::new("launch_job"),
            arguments_ref,
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        };
        let output = invoke_workflow_tool(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke start-on-call");

        let invocation_id = WorkflowToolInvocationId::for_call(
            binding.session_universe_id,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call.call_id,
            &binding.binding_fingerprint,
        );
        assert_eq!(
            output.output_json["executionId"],
            json!(workflow_tool_execution_id(
                &invocation_id,
                "wtr:sha256:recipe"
            ))
        );
        assert!(
            output.output_json["promise"].is_string(),
            "start-on-call acknowledgement carries the reply promise"
        );
        assert!(
            !output.model_visible_text.contains("executionId"),
            "execution ids are client diagnostics, not model input"
        );

        let joined_binding = WorkflowToolBinding::admit(
            binding.session_universe_id,
            binding.definition.clone(),
            binding.target.clone(),
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            },
        )
        .expect("start Joined binding");
        let joined_output = invoke_workflow_tool(
            blobs.as_ref(),
            &joined_binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
            None,
            &PromiseIdAllocator::new(1),
            1_000,
        )
        .await
        .expect("invoke start Joined call");
        assert!(joined_output.output_json["executionId"].is_string());
        assert!(joined_output.output_json.get("promises").is_none());
        assert!(
            joined_output.effects[0]
                .data
                .get("completion_promises")
                .is_some_and(|promises| promises.contains(REPLY_COMPLETION_KEY))
        );
    }

    #[tokio::test]
    async fn invalid_arguments_are_an_ordinary_tool_error() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = binding(blobs.as_ref()).await;
        let arguments_ref = blobs
            .put_bytes(br#"{"status":4}"#.to_vec())
            .await
            .expect("arguments");
        let error = validate_workflow_tool_arguments(
            blobs.as_ref(),
            &binding,
            &ToolInvocationRequest {
                builtin: None,
                call_id: ToolCallId::new("invalid-call"),
                tool_id: Some(binding.definition.tool.name.clone()),
                tool_name: binding.definition.tool.name.clone(),
                arguments_ref,
                workflow_tool: None,
                promise_control: None,
                remote_mcp: None,
            },
        )
        .await
        .expect_err("schema mismatch");
        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    #[tokio::test]
    async fn registration_installs_admitted_workflow_definition() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = binding(blobs.as_ref()).await;
        let mut toolset = register_toolset(&ToolsetConfig::empty()).expect("empty toolset");

        register_workflow_tools(&mut toolset, [&binding]).expect("materialize port");

        assert_eq!(
            toolset.tools.get(&binding.definition.tool.name),
            Some(&binding.definition.tool)
        );
        assert!(register_workflow_tools(&mut toolset, [&binding]).is_err());
    }
}
