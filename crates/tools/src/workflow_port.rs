//! Runtime adapter for notify-only workflow function ports.

use engine::{
    BlobRef, RunId, SessionId, ToolBatchId, ToolInvocationRequest, ToolKind, TurnId,
    WorkflowToolInvocation, WorkflowToolInvocationId, WorkflowToolPortBinding,
    WorkflowToolPortDefinition, storage::BlobStore, workflow_port_emit_effect,
};
use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::{ToolBinding, ToolDispatchMode, ToolInvocationOutput},
};

pub fn workflow_port_tool_binding(binding: &WorkflowToolPortBinding) -> ToolBinding {
    ToolBinding::new(
        binding.definition.tool.name.clone(),
        binding.definition.port_id.as_str(),
        ToolDispatchMode::WorkflowPort {
            port_id: binding.definition.port_id.clone(),
            binding_fingerprint: binding.binding_fingerprint.clone(),
        },
        binding.definition.tool.parallelism,
    )
}

/// Validate every CAS document needed to present and invoke a port.
pub async fn validate_workflow_port_definition_documents(
    blobs: &dyn BlobStore,
    definition: &WorkflowToolPortDefinition,
) -> ToolResult<()> {
    let ToolKind::Function(function) = &definition.tool.kind else {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "workflow port {} is not a function tool",
                definition.port_id
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

/// Validate one CAS-backed argument object against its admitted input schema.
pub async fn validate_workflow_port_arguments(
    blobs: &dyn BlobStore,
    binding: &WorkflowToolPortBinding,
    arguments_ref: &BlobRef,
) -> ToolResult<Value> {
    let arguments = read_json(blobs, arguments_ref, "arguments").await?;
    let ToolKind::Function(function) = &binding.definition.tool.kind else {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "workflow port {} is not a function tool",
                binding.definition.port_id
            ),
        });
    };
    let schema = read_json(blobs, &function.input_schema_ref, "input schema").await?;
    let validator =
        jsonschema::validator_for(&schema).map_err(|error| ToolError::InvalidRequest {
            message: format!(
                "workflow port {} has an unsupported input schema: {error}",
                binding.definition.port_id
            ),
        })?;
    if let Err(error) = validator.validate(&arguments) {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "arguments for workflow port {} do not match its input schema: {error}",
                binding.definition.port_id
            ),
        });
    }
    Ok(arguments)
}

/// Build the stable acknowledgement and trusted internal effect for a valid
/// workflow-port call. The caller persists the ordinary tool output.
#[allow(clippy::too_many_arguments)]
pub async fn invoke_workflow_port(
    blobs: &dyn BlobStore,
    binding: &WorkflowToolPortBinding,
    session_id: &SessionId,
    run_id: RunId,
    turn_id: TurnId,
    tool_batch_id: ToolBatchId,
    call: &ToolInvocationRequest,
) -> ToolResult<ToolInvocationOutput> {
    binding
        .validate()
        .map_err(|error| ToolError::InvalidRequest {
            message: format!(
                "invalid workflow port {} binding: {error}",
                binding.definition.port_id
            ),
        })?;
    if call.tool_name != binding.definition.tool.name {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "workflow port {} is bound to tool {}, got {}",
                binding.definition.port_id, binding.definition.tool.name, call.tool_name
            ),
        });
    }
    if call.execution_target.is_some() {
        return Err(ToolError::InvalidRequest {
            message: format!(
                "workflow port {} must not have an execution target",
                binding.definition.port_id
            ),
        });
    }
    validate_workflow_port_arguments(blobs, binding, &call.arguments_ref).await?;

    let invocation_id = WorkflowToolInvocationId::for_call(
        binding.session_universe_id,
        session_id,
        run_id,
        turn_id,
        tool_batch_id,
        &call.call_id,
        &binding.binding_fingerprint,
    );
    let invocation = WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        port_id: binding.definition.port_id.clone(),
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
        reply_promise_id: None,
    };
    let acknowledgement = json!({
        "accepted": true,
        "invocationId": invocation_id.as_str(),
    });
    Ok(ToolInvocationOutput {
        model_visible_text: acknowledgement.to_string(),
        output_json: acknowledgement,
        effects: vec![workflow_port_emit_effect(&invocation)],
    })
}

async fn compile_schema(blobs: &dyn BlobStore, schema_ref: &BlobRef, kind: &str) -> ToolResult<()> {
    let schema = read_json(blobs, schema_ref, &format!("{kind} schema")).await?;
    jsonschema::validator_for(&schema).map_err(|error| ToolError::InvalidRequest {
        message: format!("workflow port {kind} schema is unsupported: {error}"),
    })?;
    Ok(())
}

async fn read_json(blobs: &dyn BlobStore, blob_ref: &BlobRef, kind: &str) -> ToolResult<Value> {
    let bytes = blobs.read_bytes(blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| ToolError::InvalidRequest {
        message: format!("workflow port {kind} is not valid JSON: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        FunctionToolSpec, ToolCallId, ToolKind, ToolName, ToolParallelism, ToolSpec,
        ToolTargetRequirement, WorkflowEndpointRef, WorkflowToolPortId,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::toolset::{
        ToolsetConfig, ToolsetEnvironment, materialize_workflow_ports, resolve_toolset,
    };

    async fn binding(blobs: &dyn BlobStore) -> WorkflowToolPortBinding {
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
        WorkflowToolPortBinding::admit(
            Uuid::from_u128(1),
            WorkflowToolPortDefinition {
                port_id: WorkflowToolPortId::new("report"),
                revision: 1,
                semantic_type: "lightspeed.work.report.v1".to_owned(),
                tool: ToolSpec {
                    name: ToolName::new("work_report"),
                    kind: ToolKind::Function(FunctionToolSpec {
                        model_name: None,
                        description_ref: None,
                        input_schema_ref: schema_ref,
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: ToolParallelism::ParallelSafe,
                    target_requirement: ToolTargetRequirement::None,
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
            call_id: ToolCallId::new("call-1"),
            tool_name: ToolName::new("work_report"),
            arguments_ref,
            execution_target: None,
        };
        let first = invoke_workflow_port(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
        )
        .await
        .expect("invoke");
        let retry = invoke_workflow_port(
            blobs.as_ref(),
            &binding,
            &SessionId::new("session-1"),
            RunId::new(1),
            TurnId::new(2),
            ToolBatchId::new(3),
            &call,
        )
        .await
        .expect("retry");

        assert_eq!(first, retry);
        assert_eq!(first.effects.len(), 1);
        assert_eq!(
            first.effects[0].kind,
            engine::WORKFLOW_PORT_EMIT_EFFECT_KIND
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
        let error = validate_workflow_port_arguments(blobs.as_ref(), &binding, &arguments_ref)
            .await
            .expect_err("schema mismatch");
        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    #[tokio::test]
    async fn materialization_installs_function_spec_and_runtime_binding() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let binding = binding(blobs.as_ref()).await;
        let target = crate::runtime::ToolTarget::api_kind(engine::ProviderApiKind::OpenAiResponses);
        let mut toolset = resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::empty(),
        )
        .expect("empty toolset");

        materialize_workflow_ports(&mut toolset, [&binding]).expect("materialize port");

        assert_eq!(
            toolset.tools.get(&binding.definition.tool.name),
            Some(&binding.definition.tool)
        );
        let runtime_binding = toolset
            .catalog
            .get(&binding.definition.tool.name)
            .expect("runtime binding");
        assert_eq!(
            runtime_binding.dispatch,
            ToolDispatchMode::WorkflowPort {
                port_id: binding.definition.port_id.clone(),
                binding_fingerprint: binding.binding_fingerprint.clone(),
            }
        );
        assert!(materialize_workflow_ports(&mut toolset, [&binding]).is_err());
    }
}
