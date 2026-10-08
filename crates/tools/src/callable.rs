//! Shared resolved tool specifications and their matching execution bindings.
//!
//! Provider adapters render these definitions into their own wire format. A
//! script catalog selects only `into_callable()` entries and retains the whole
//! pair, so a later change of model cannot silently change its argument adapter.
//! These records describe capabilities; they do not grant authority to invoke
//! them. The owning session must still admit every call.

use std::collections::BTreeSet;

use harness::{
    BlobRef, ProviderApiKind, ProviderNativeToolExecution, RemoteMcpToolSpec, ToolKind, ToolName,
    ToolSpec, storage::BlobStore,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    ToolError,
    definitions::{self, Definition},
    runtime::{FunctionDefinition, ToolBinding, ToolInvocationOutput, ToolTarget},
};

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    BlobStore(#[from] harness::storage::BlobStoreError),
    #[error("blob {blob_ref} is not valid UTF-8: {message}")]
    InvalidUtf8 { blob_ref: BlobRef, message: String },
    #[error("invalid JSON in blob {blob_ref}: {message}")]
    InvalidJson { blob_ref: BlobRef, message: String },
    #[error("invalid tool catalog: {message}")]
    InvalidCatalog { message: String },
}

#[derive(Clone, Debug)]
pub struct ResolvedTool {
    pub id: ToolName,
    pub name: ToolName,
    pub kind: ResolvedToolKind,
    pub callable_binding: Option<CallableBinding>,
}

#[derive(Clone, Debug)]
pub enum ResolvedToolKind {
    Function(FunctionDefinition),
    ProviderNative(NativeDefinition),
    RemoteMcp(RemoteMcpToolSpec),
}

#[derive(Clone, Debug)]
pub struct NativeDefinition {
    pub api_kind: ProviderApiKind,
    pub definition: Value,
    pub execution: ProviderNativeToolExecution,
}

/// Host-side routing metadata. Never accept this from generated source: the
/// session pins it behind an opaque execution-scoped handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallableBinding {
    Builtin {
        binding: ToolBinding,
    },
    /// An authored function, including a workflow-backed function. Its admitted
    /// registration owns execution; the interpreter does not choose a backend.
    Function,
}

/// The function specification and execution adapter must be retained together.
/// `definition.output_schema`, when known, describes the successful script
/// value. Failures reject the guest promise with their structured output intact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallableTool {
    pub tool_id: ToolName,
    pub definition: CallableSpecification,
    pub binding: CallableBinding,
    /// A workflow binding changes completion semantics. Retain its admitted
    /// fingerprint without copying destinations or authority into the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_binding_fingerprint: Option<String>,
}

/// Script-facing metadata projected from the same function definition used by
/// the model adapter. Provider transport options stay outside this snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallableSpecification {
    pub name: ToolName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

/// Resolve a workflow callable directly from its admitted definition. This
/// prevents combining an old argument adapter with a newer completion binding
/// that happens to use the same name.
pub async fn resolve_workflow(
    blobs: &dyn BlobStore,
    target: &ToolTarget,
    workflow: &harness::WorkflowToolBinding,
) -> Result<Vec<CallableTool>, CatalogError> {
    let resolved = resolve(
        blobs,
        target,
        std::slice::from_ref(&workflow.definition.tool),
    )
    .await?;
    let mut callables = Vec::with_capacity(resolved.len());
    for tool in resolved {
        let mut callable = tool.into_callable().ok_or_else(|| {
            invalid("workflow binding requires a host-callable function".to_owned())
        })?;
        callable.definition.output_schema = workflow_result_schema(
            blobs,
            &workflow.into(),
            matches!(workflow.definition.tool.kind, ToolKind::Function(_)),
            callable.definition.output_schema,
        )
        .await?;
        callable.workflow_binding_fingerprint = Some(workflow.binding_fingerprint.clone());
        callables.push(callable);
    }
    Ok(callables)
}

/// Share the exact result contract between model presentation and script binding.
pub async fn workflow_result_schema(
    blobs: &dyn BlobStore,
    contract: &harness::WorkflowToolResultContract,
    authored_function: bool,
    declared_schema: Option<Value>,
) -> Result<Option<Value>, CatalogError> {
    Ok(match &contract.completion {
        harness::WorkflowToolCompletion::Joined {
            reply_schema_ref, ..
        } => {
            match reply_schema_ref {
                Some(reference) => Some(read_json(blobs, reference).await?),
                // An authored function may declare its final result. A
                // substrate's schema does not describe an arbitrary
                // workflow bound to the same built-in operation.
                None if authored_function => declared_schema,
                None => None,
            }
        }
        completion => crate::workflow_tool::acknowledgement_result_schema(
            completion,
            contract.starts_workflow,
        ),
    })
}

impl ResolvedTool {
    pub fn into_callable(self) -> Option<CallableTool> {
        match (self.kind, self.callable_binding) {
            (ResolvedToolKind::Function(definition), Some(binding)) => Some(CallableTool {
                tool_id: self.id,
                definition: CallableSpecification {
                    name: definition.name,
                    description: definition.description,
                    input_schema: definition.input_schema,
                    output_schema: definition.output_schema,
                },
                binding,
                workflow_binding_fingerprint: None,
            }),
            // A client-effect native tool can still lack a JSON-callable
            // adapter. Provider-hosted tools and remote inventories also do
            // not become callable merely by appearing in a model request.
            _ => None,
        }
    }
}

/// Plain data at the script bridge boundary. Success settles with `value`
/// directly; failure rejects with `message` and optional tool output. In
/// particular, an MCP error retains its content/structuredContent envelope.
/// Effects and attachment admission remain the host's responsibility and are
/// never serialized through this type. Scoped handles can occur within value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ScriptToolResult {
    Succeeded {
        value: Value,
    },
    Failed {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
}

impl ScriptToolResult {
    /// Borrow the runtime result so the caller keeps its effects and attachments
    /// for session application and artifact admission.
    pub fn succeeded(output: &ToolInvocationOutput) -> Self {
        Self::Succeeded {
            value: output.output_json.clone(),
        }
    }
}

/// Resolve the same metadata for direct model tools and script catalog entries.
/// Remote inventories stay deferred; resolving this catalog does not perform
/// discovery or copy any remote credentials into callable bindings.
pub async fn resolve(
    blobs: &dyn BlobStore,
    target: &ToolTarget,
    tools: &[ToolSpec],
) -> Result<Vec<ResolvedTool>, CatalogError> {
    let mut resolved = Vec::new();
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for tool in tools {
        if !ids.insert(&tool.name) {
            return Err(invalid(format!(
                "duplicate tool registration {}",
                tool.name
            )));
        }
        match &tool.kind {
            ToolKind::Builtin(spec) => {
                for builtin in definitions::resolve(&tool.name, spec, target)? {
                    let kind = match builtin.definition {
                        Definition::Function(function) => ResolvedToolKind::Function(function),
                        Definition::Native(definition) => {
                            ResolvedToolKind::ProviderNative(NativeDefinition {
                                api_kind: target.api_kind.clone(),
                                definition,
                                execution: ProviderNativeToolExecution::ProviderHosted,
                            })
                        }
                    };
                    resolved.push(ResolvedTool {
                        id: tool.name.clone(),
                        name: builtin.name,
                        kind,
                        callable_binding: builtin
                            .binding
                            .map(|binding| CallableBinding::Builtin { binding }),
                    });
                }
            }
            ToolKind::Function(function) => {
                resolved.push(ResolvedTool {
                    id: tool.name.clone(),
                    name: tool.name.clone(),
                    kind: ResolvedToolKind::Function(FunctionDefinition {
                        name: tool.name.clone(),
                        description: match &function.description_ref {
                            Some(reference) => Some(read_text(blobs, reference).await?),
                            None => None,
                        },
                        input_schema: read_json(blobs, &function.input_schema_ref).await?,
                        output_schema: match &function.output_schema_ref {
                            Some(reference) => Some(read_json(blobs, reference).await?),
                            None => None,
                        },
                        strict: function.strict,
                        provider_options: match &function.provider_options_ref {
                            Some(reference) => Some(read_json(blobs, reference).await?),
                            None => None,
                        },
                    }),
                    callable_binding: Some(CallableBinding::Function),
                });
            }
            ToolKind::ProviderNative(native) => {
                let definition = read_json(blobs, &native.native_tool_ref).await?;
                let name = match definition.get("name").and_then(Value::as_str) {
                    Some(name) => {
                        ToolName::try_new(name).map_err(|error| invalid(error.to_string()))?
                    }
                    None => tool.name.clone(),
                };
                resolved.push(ResolvedTool {
                    id: tool.name.clone(),
                    name,
                    kind: ResolvedToolKind::ProviderNative(NativeDefinition {
                        api_kind: native.api_kind.clone(),
                        definition,
                        execution: native.execution.clone(),
                    }),
                    callable_binding: None,
                });
            }
            ToolKind::RemoteMcp(remote) => resolved.push(ResolvedTool {
                id: tool.name.clone(),
                name: tool.name.clone(),
                kind: ResolvedToolKind::RemoteMcp(remote.clone()),
                callable_binding: None,
            }),
        }
    }
    for tool in &resolved {
        if let ResolvedToolKind::Function(function) = &tool.kind {
            validate_provider_options(function)?;
        }
        if matches!(tool.kind, ResolvedToolKind::RemoteMcp(_)) {
            continue;
        }
        if !valid_exposed_name(tool.name.as_str()) {
            return Err(invalid(format!("invalid exposed tool name {}", tool.name)));
        }
        if !names.insert(&tool.name) {
            return Err(invalid(format!(
                "duplicate exposed tool name {}",
                tool.name
            )));
        }
    }
    // Preserve existing provider-visible order for mixed/built-in registries.
    if tools
        .iter()
        .any(|tool| matches!(tool.kind, ToolKind::Builtin(_)))
    {
        resolved.sort_by(|left, right| left.name.cmp(&right.name));
    }
    Ok(resolved)
}

fn validate_provider_options(function: &FunctionDefinition) -> Result<(), CatalogError> {
    let Some(options) = &function.provider_options else {
        return Ok(());
    };
    let options = options.as_object().ok_or_else(|| {
        invalid(format!(
            "provider options for tool {} must be a JSON object",
            function.name
        ))
    })?;
    // Extensions must not create duplicate wire fields that replace the
    // specification paired with this binding. Formatting stays in llm-runtime.
    for key in [
        "name",
        "description",
        "parameters",
        "input_schema",
        "strict",
        "type",
        "function",
    ] {
        if options.contains_key(key) {
            return Err(invalid(format!(
                "provider options for tool {} cannot override {key}",
                function.name
            )));
        }
    }
    Ok(())
}

pub fn valid_exposed_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn invalid(message: String) -> CatalogError {
    CatalogError::InvalidCatalog { message }
}

async fn read_text(blobs: &dyn BlobStore, reference: &BlobRef) -> Result<String, CatalogError> {
    String::from_utf8(blobs.read_bytes(reference).await?).map_err(|error| {
        CatalogError::InvalidUtf8 {
            blob_ref: reference.clone(),
            message: error.to_string(),
        }
    })
}

async fn read_json(blobs: &dyn BlobStore, reference: &BlobRef) -> Result<Value, CatalogError> {
    serde_json::from_slice(&blobs.read_bytes(reference).await?).map_err(|error| {
        CatalogError::InvalidJson {
            blob_ref: reference.clone(),
            message: error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{FunctionToolSpec, ToolParallelism, storage::InMemoryBlobStore};
    use serde_json::json;

    fn builtin(id: &str) -> ToolSpec {
        definitions::register(
            id,
            Default::default(),
            ToolParallelism::ParallelSafe,
            Default::default(),
        )
    }

    async fn function(blobs: &dyn BlobStore, output: Option<Value>) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("lookup"),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: Some(blobs.put_bytes(b"Find one item".to_vec()).await.unwrap()),
                input_schema_ref: blobs
                    .put_bytes(br#"{"type":"object"}"#.to_vec())
                    .await
                    .unwrap(),
                output_schema_ref: match output {
                    Some(schema) => Some(
                        blobs
                            .put_bytes(serde_json::to_vec(&schema).unwrap())
                            .await
                            .unwrap(),
                    ),
                    None => None,
                },
                strict: Some(true),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            execution: Default::default(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authored_output_schema_is_preserved_and_unknown_is_callable() {
        let blobs = InMemoryBlobStore::new();
        let schema =
            json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]});
        for expected in [Some(schema), None] {
            let tool = function(&blobs, expected.clone()).await;
            let mut catalog = resolve(
                &blobs,
                &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                &[tool],
            )
            .await
            .unwrap();
            let callable = catalog.pop().unwrap().into_callable().unwrap();
            assert_eq!(callable.definition.output_schema, expected);
            assert_eq!(
                callable.definition.description.as_deref(),
                Some("Find one item")
            );
            assert_eq!(callable.binding, CallableBinding::Function);
            let bytes = serde_json::to_vec(&callable).unwrap();
            assert_eq!(
                serde_json::from_slice::<CallableTool>(&bytes).unwrap(),
                callable
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_presentations_pin_matching_argument_adapters() {
        let blobs = InMemoryBlobStore::new();
        let registrations = [builtin("env.run_process")];
        let mut retained = Vec::new();
        for (api, name, argument) in [
            (ProviderApiKind::AnthropicMessages, "Bash", "command"),
            (ProviderApiKind::OpenAiResponses, "exec_command", "cmd"),
        ] {
            let mut resolved = resolve(&blobs, &ToolTarget::api_kind(api), &registrations)
                .await
                .unwrap();
            let callable = resolved.pop().unwrap().into_callable().unwrap();
            assert_eq!(callable.definition.name.as_str(), name);
            assert!(
                callable.definition.input_schema["properties"]
                    .get(argument)
                    .is_some()
            );
            let CallableBinding::Builtin { binding } = &callable.binding else {
                panic!("expected builtin binding")
            };
            assert_eq!(binding.tool_name, callable.definition.name);
            assert_eq!(binding.logical_id, "env.run_process");
            assert!(binding.adapter_id.is_some());
            retained.push(callable);
        }
        assert_ne!(retained[0].binding, retained[1].binding);
        // Resolving the second provider did not rebind the previously published spec.
        assert_eq!(retained[0].definition.name.as_str(), "Bash");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hosted_and_opaque_native_tools_do_not_grant_script_capabilities() {
        let blobs = InMemoryBlobStore::new();
        let native_ref = blobs
            .put_bytes(br#"{"type":"custom","name":"native_effect"}"#.to_vec())
            .await
            .unwrap();
        let native = ToolSpec {
            name: ToolName::new("native_effect"),
            kind: ToolKind::ProviderNative(harness::ProviderNativeToolSpec {
                api_kind: ProviderApiKind::AnthropicMessages,
                native_tool_ref: native_ref,
                execution: ProviderNativeToolExecution::ClientEffect,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            execution: Default::default(),
        };
        let tools = resolve(
            &blobs,
            &ToolTarget::api_kind(ProviderApiKind::AnthropicMessages),
            &[builtin("web.search"), builtin("web.fetch"), native],
        )
        .await
        .unwrap();
        assert_eq!(tools.len(), 3);
        assert!(tools.into_iter().all(|tool| tool.into_callable().is_none()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_exposed_names_cannot_produce_ambiguous_bindings() {
        let blobs = InMemoryBlobStore::new();
        let mut authored = function(&blobs, None).await;
        authored.name = ToolName::new("exec_command");
        assert!(matches!(
            resolve(
                &blobs,
                &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                &[builtin("env.run_process"), authored]
            )
            .await,
            Err(CatalogError::InvalidCatalog { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_output_schema_json_fails_before_publication() {
        let blobs = InMemoryBlobStore::new();
        let mut tool = function(&blobs, None).await;
        let ToolKind::Function(spec) = &mut tool.kind else {
            unreachable!()
        };
        spec.output_schema_ref = Some(blobs.put_bytes(b"not JSON".to_vec()).await.unwrap());
        assert!(matches!(
            resolve(
                &blobs,
                &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                &[tool]
            )
            .await,
            Err(CatalogError::InvalidJson { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_extensions_cannot_replace_the_pinned_contract() {
        let blobs = InMemoryBlobStore::new();
        for key in [
            "name",
            "description",
            "parameters",
            "input_schema",
            "strict",
            "type",
            "function",
            "cache_control",
        ] {
            let mut tool = function(&blobs, None).await;
            let ToolKind::Function(spec) = &mut tool.kind else {
                unreachable!()
            };
            spec.provider_options_ref = Some(
                blobs
                    .put_bytes(serde_json::to_vec(&json!({key: {"type":"ephemeral"}})).unwrap())
                    .await
                    .unwrap(),
            );
            let result = resolve(
                &blobs,
                &ToolTarget::api_kind(ProviderApiKind::AnthropicMessages),
                &[tool],
            )
            .await;
            if key == "cache_control" {
                assert!(result.is_ok(), "unrelated provider options are retained");
            } else {
                assert!(
                    matches!(result, Err(CatalogError::InvalidCatalog { .. })),
                    "{key}"
                );
            }
        }
    }

    fn workflow_binding(
        tool: ToolSpec,
        completion: harness::WorkflowToolCompletion,
    ) -> harness::WorkflowToolBinding {
        harness::WorkflowToolBinding::admit(
            uuid::Uuid::from_u128(1),
            harness::WorkflowToolDefinition {
                tool_id: harness::WorkflowToolId::new("lookup"),
                revision: 1,
                semantic_type: "test.lookup.v1".to_owned(),
                tool,
            },
            harness::WorkflowToolTarget::Bound {
                receiver: harness::WorkflowEndpointRef {
                    workflow_id: "lookup-worker".to_owned(),
                    workflow_kind: "lookup".to_owned(),
                },
                dispatch: harness::BoundWorkflowToolDispatch::Push,
            },
            completion,
        )
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workflow_spec_comes_from_the_binding_and_describes_its_completion() {
        let blobs = InMemoryBlobStore::new();
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiResponses);
        let mut registered = function(&blobs, Some(json!({"type":"array"}))).await;
        let ToolKind::Function(spec) = &mut registered.kind else {
            unreachable!()
        };
        spec.input_schema_ref = blobs
            .put_bytes(br#"{"type":"object","properties":{"current":{"type":"boolean"}}}"#.to_vec())
            .await
            .unwrap();
        let binding = workflow_binding(
            registered.clone(),
            harness::WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: None,
                max_promises: 1,
                key_source: harness::WorkflowToolCompletionKeySource::Reply,
            },
        );
        let callable = resolve_workflow(&blobs, &target, &binding)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert!(
            callable.definition.input_schema["properties"]
                .get("current")
                .is_some()
        );
        assert_eq!(
            callable.workflow_binding_fingerprint.as_deref(),
            Some(binding.binding_fingerprint.as_str())
        );
        let schema = callable.definition.output_schema.unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(
            validator.is_valid(
                &json!({"accepted":true,"invocationId":"invocation","promise":"promise_1"})
            )
        );
        assert!(
            !validator.is_valid(&json!([])),
            "submission returns an acknowledgement, not the later reply"
        );

        let reply_schema =
            json!({"type":"object","required":["rows"],"properties":{"rows":{"type":"array"}}});
        let reply_ref = blobs
            .put_bytes(serde_json::to_vec(&reply_schema).unwrap())
            .await
            .unwrap();
        let joined = workflow_binding(
            registered,
            harness::WorkflowToolCompletion::Joined {
                reply_schema_ref: Some(reply_ref),
                deadline_after_ms: 1000,
            },
        );
        let callable = resolve_workflow(&blobs, &target, &joined)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(callable.definition.output_schema, Some(reply_schema));
        assert_ne!(
            callable.workflow_binding_fingerprint.as_deref(),
            Some(binding.binding_fingerprint.as_str())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn arbitrary_joined_workflow_does_not_inherit_substrate_output_schema() {
        let blobs = InMemoryBlobStore::new();
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiResponses);
        let tool = builtin("env.job_run");
        let ordinary = resolve(&blobs, &target, std::slice::from_ref(&tool))
            .await
            .unwrap()
            .pop()
            .unwrap()
            .into_callable()
            .unwrap();
        assert!(ordinary.definition.output_schema.is_some());
        let binding = workflow_binding(
            tool,
            harness::WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 1000,
            },
        );
        let callable = resolve_workflow(&blobs, &target, &binding)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert!(callable.definition.output_schema.is_none());
    }

    #[test]
    fn script_projection_preserves_structured_data_without_host_envelope() {
        let value =
            json!({"content":[{"type":"text","text":"two items"}],"structuredContent":{"count":2}});
        let output = ToolInvocationOutput {
            output_json: value.clone(),
            model_visible_text: "provider-formatted prose".into(),
            effects: vec![],
            attachments: vec![],
        };
        assert_eq!(
            ScriptToolResult::succeeded(&output),
            ScriptToolResult::Succeeded {
                value: value.clone()
            }
        );
        let failure = ScriptToolResult::Failed {
            message: "remote tool failed".into(),
            value: Some(value),
        };
        let serialized = serde_json::to_value(&failure).unwrap();
        assert_eq!(serialized["status"], "failed");
        assert_eq!(serialized["value"]["structuredContent"]["count"], 2);
        assert!(serialized.get("effects").is_none());
        assert!(serialized.get("model_visible_text").is_none());
    }
}
