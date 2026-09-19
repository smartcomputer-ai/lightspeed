//! Runtime tool presentation and the reverse lookup for one provider request.

use std::collections::{BTreeMap, BTreeSet};

use engine::{
    LlmGenerationResult, ProviderApiKind, ProviderNativeToolExecution, RemoteMcpToolSpec,
    ToolChoice, ToolKind, ToolName, ToolSpec, storage::BlobStore,
};
use serde_json::Value;
use tools::{
    definitions,
    runtime::{FunctionDefinition, ToolTarget},
};

use crate::{
    blob_io::{read_json, read_text},
    error::{LlmAdapterError, LlmAdapterResult},
};

#[derive(Clone, Debug)]
pub(crate) struct ResolvedTool {
    pub id: ToolName,
    pub name: ToolName,
    pub kind: ResolvedToolKind,
}

#[derive(Clone, Debug)]
pub(crate) enum ResolvedToolKind {
    Function(FunctionDefinition),
    ProviderNative(NativeDefinition),
    RemoteMcp(RemoteMcpToolSpec),
}

#[derive(Clone, Debug)]
pub(crate) struct NativeDefinition {
    pub api_kind: ProviderApiKind,
    pub definition: Value,
    pub execution: ProviderNativeToolExecution,
}

pub(crate) struct ToolCatalog {
    pub tools: Vec<ResolvedTool>,
    pub names: AdvertisedNames,
    primary_names: BTreeMap<ToolName, ToolName>,
}

impl ToolCatalog {
    pub async fn resolve(
        blobs: &dyn BlobStore,
        target: &ToolTarget,
        tools: &[ToolSpec],
    ) -> LlmAdapterResult<Self> {
        let mut catalog = Self {
            tools: Vec::new(),
            names: AdvertisedNames::default(),
            primary_names: BTreeMap::new(),
        };
        let mut ids = BTreeSet::new();
        for tool in tools {
            if !ids.insert(&tool.name) {
                return Err(LlmAdapterError::InvalidProviderRequest {
                    message: format!("duplicate tool registration {}", tool.name),
                });
            }
            match &tool.kind {
                ToolKind::Builtin(spec) => {
                    for resolved in
                        definitions::resolve(&tool.name, spec, target).map_err(|error| {
                            LlmAdapterError::InvalidProviderRequest {
                                message: error.to_string(),
                            }
                        })?
                    {
                        let kind = match resolved.definition {
                            definitions::Definition::Function(function) => {
                                ResolvedToolKind::Function(function)
                            }
                            definitions::Definition::Native(definition) => {
                                ResolvedToolKind::ProviderNative(NativeDefinition {
                                    api_kind: target.api_kind.clone(),
                                    definition,
                                    execution: ProviderNativeToolExecution::ProviderHosted,
                                })
                            }
                        };
                        catalog.push(ResolvedTool {
                            id: tool.name.clone(),
                            name: resolved.name,
                            kind,
                        })?;
                    }
                }
                ToolKind::Function(function) => {
                    let definition = FunctionDefinition {
                        name: tool.name.clone(),
                        description: match &function.description_ref {
                            Some(reference) => Some(read_text(blobs, reference).await?),
                            None => None,
                        },
                        input_schema: read_json(blobs, &function.input_schema_ref).await?,
                        strict: function.strict,
                        provider_options: match &function.provider_options_ref {
                            Some(reference) => Some(read_json(blobs, reference).await?),
                            None => None,
                        },
                    };
                    catalog.push(ResolvedTool {
                        id: tool.name.clone(),
                        name: tool.name.clone(),
                        kind: ResolvedToolKind::Function(definition),
                    })?;
                }
                ToolKind::ProviderNative(native) => {
                    let definition = read_json(blobs, &native.native_tool_ref).await?;
                    let name = match definition.get("name").and_then(Value::as_str) {
                        Some(name) => ToolName::try_new(name).map_err(|error| {
                            LlmAdapterError::InvalidProviderRequest {
                                message: error.to_string(),
                            }
                        })?,
                        None => tool.name.clone(),
                    };
                    catalog.push(ResolvedTool {
                        id: tool.name.clone(),
                        name,
                        kind: ResolvedToolKind::ProviderNative(NativeDefinition {
                            api_kind: native.api_kind.clone(),
                            definition,
                            execution: native.execution.clone(),
                        }),
                    })?;
                }
                ToolKind::RemoteMcp(remote) => catalog.push(ResolvedTool {
                    id: tool.name.clone(),
                    name: tool.name.clone(),
                    kind: ResolvedToolKind::RemoteMcp(remote.clone()),
                })?,
            }
        }
        // Preserve the previous provider-visible BTreeMap ordering, even though
        // the admitted registry is now ordered by logical identity.
        if tools
            .iter()
            .any(|tool| matches!(tool.kind, ToolKind::Builtin(_)))
        {
            catalog
                .tools
                .sort_by(|left, right| left.name.cmp(&right.name));
        }
        Ok(catalog)
    }

    fn push(&mut self, tool: ResolvedTool) -> LlmAdapterResult<()> {
        let client = matches!(
            tool.kind,
            ResolvedToolKind::Function(_)
                | ResolvedToolKind::ProviderNative(NativeDefinition {
                    execution: ProviderNativeToolExecution::ClientEffect,
                    ..
                })
        );
        if !matches!(tool.kind, ResolvedToolKind::RemoteMcp(_)) {
            self.primary_names
                .entry(tool.id.clone())
                .or_insert_with(|| tool.name.clone());
            self.names
                .insert(tool.name.clone(), client.then_some(tool.id.clone()))?;
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn tool_choice(&self, choice: Option<&ToolChoice>) -> LlmAdapterResult<Option<ToolChoice>> {
        Ok(match choice {
            Some(ToolChoice::Specific { tool_name }) => Some(ToolChoice::Specific {
                tool_name: self.primary_names.get(tool_name).cloned().ok_or_else(|| {
                    LlmAdapterError::InvalidProviderRequest {
                        message: format!("tool choice {tool_name} has no available presentation"),
                    }
                })?,
            }),
            other => other.cloned(),
        })
    }

    pub fn normalize(&self, result: &mut LlmGenerationResult) {
        for call in &mut result.facts.tool_calls {
            call.tool_id = self.names.call_ids.get(&call.tool_name).cloned();
        }
    }
}

/// The exact function namespace advertised in this request, including expanded
/// MCP functions and provider-hosted helpers. Only client functions can route
/// back into the engine.
#[derive(Default)]
pub(crate) struct AdvertisedNames {
    exposed: BTreeSet<ToolName>,
    call_ids: BTreeMap<ToolName, ToolName>,
}

impl AdvertisedNames {
    pub fn insert(&mut self, name: ToolName, id: Option<ToolName>) -> LlmAdapterResult<()> {
        if !valid_exposed_name(name.as_str()) {
            return Err(LlmAdapterError::InvalidProviderRequest {
                message: format!("invalid exposed tool name {name}"),
            });
        }
        if !self.exposed.insert(name.clone()) {
            return Err(LlmAdapterError::InvalidProviderRequest {
                message: format!("duplicate exposed tool name {name}"),
            });
        }
        if let Some(id) = id {
            self.call_ids.insert(name, id);
        }
        Ok(())
    }
}

pub(crate) fn valid_exposed_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{
        BlobRef, BuiltinToolSpec, FunctionToolSpec, LlmFinish, LlmGenerationFacts,
        LlmGenerationStatus, ObservedToolCall, RunId, ToolCallId, ToolParallelism, TurnId,
        storage::InMemoryBlobStore,
    };
    use serde_json::json;

    fn builtin(id: &str) -> ToolSpec {
        definitions::register(
            id,
            Default::default(),
            ToolParallelism::Exclusive,
            Default::default(),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_registration_uses_the_turn_model_and_rejects_unadvertised_aliases() {
        let blobs = InMemoryBlobStore::new();
        let registration = builtin("env.continue_process");
        let claude = ToolCatalog::resolve(
            &blobs,
            &ToolTarget::api_kind(ProviderApiKind::AnthropicMessages),
            std::slice::from_ref(&registration),
        )
        .await
        .expect("Claude catalog");
        let codex = ToolCatalog::resolve(
            &blobs,
            &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
            &[registration],
        )
        .await
        .expect("Codex catalog");
        let mut result = LlmGenerationResult {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: Vec::new(),
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: None,
                finish: LlmFinish::ToolCalls,
                usage: None,
                approval_requests: Vec::new(),
                context_token_estimate: None,
                tool_calls: ["BashOutput", "KillShell", "write_stdin", "continue_process"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| ObservedToolCall {
                        call_id: ToolCallId::new(format!("call_{index}")),
                        tool_name: ToolName::new(name),
                        tool_id: None,
                        provider_kind: None,
                        arguments_ref: BlobRef::from_bytes(b"{}"),
                        native_call_ref: Some(BlobRef::from_bytes(name.as_bytes())),
                    })
                    .collect(),
            },
        };
        let original = result.clone();
        claude.normalize(&mut result);
        assert_eq!(
            result
                .facts
                .tool_calls
                .iter()
                .map(|call| call.tool_id.as_ref().map(ToolName::as_str))
                .collect::<Vec<_>>(),
            [
                Some("env.continue_process"),
                Some("env.continue_process"),
                None,
                None
            ]
        );
        for (call, original) in result
            .facts
            .tool_calls
            .iter()
            .zip(&original.facts.tool_calls)
        {
            assert_eq!(call.tool_name, original.tool_name);
            assert_eq!(call.native_call_ref, original.native_call_ref);
            assert_eq!(call.arguments_ref, original.arguments_ref);
        }
        codex.normalize(&mut result);
        assert_eq!(
            result
                .facts
                .tool_calls
                .iter()
                .map(|call| call.tool_id.as_ref().map(ToolName::as_str))
                .collect::<Vec<_>>(),
            [None, None, Some("env.continue_process"), None]
        );
        let choice = ToolChoice::Specific {
            tool_name: ToolName::new("env.continue_process"),
        };
        assert_eq!(
            claude.tool_choice(Some(&choice)).unwrap(),
            Some(ToolChoice::Specific {
                tool_name: ToolName::new("BashOutput")
            })
        );
        assert_eq!(
            codex.tool_choice(Some(&choice)).unwrap(),
            Some(ToolChoice::Specific {
                tool_name: ToolName::new("write_stdin")
            })
        );
        assert!(
            claude
                .tool_choice(Some(&ToolChoice::Specific {
                    tool_name: ToolName::new("env.run_process")
                }))
                .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completions_admit_only_currently_advertised_process_and_editing_names() {
        let blobs = InMemoryBlobStore::new();
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiCompletions);
        let registrations = [
            builtin("env.run_process"),
            builtin("env.continue_process"),
            builtin("env.edit_file"),
            builtin("env.apply_patch"),
            builtin("vfs.apply_patch"),
        ];
        let catalog = ToolCatalog::resolve(&blobs, &target, &registrations)
            .await
            .unwrap();
        for (name, id) in [
            ("exec_command", "env.run_process"),
            ("write_stdin", "env.continue_process"),
            ("edit_file", "env.edit_file"),
        ] {
            assert_eq!(
                catalog.names.call_ids.get(&ToolName::new(name)),
                Some(&ToolName::new(id))
            );
        }
        for name in [
            "run_process",
            "continue_process",
            "apply_patch",
            "vfs_apply_patch",
            "Bash",
        ] {
            assert!(!catalog.names.call_ids.contains_key(&ToolName::new(name)));
        }
        for id in ["env.apply_patch", "vfs.apply_patch"] {
            assert!(matches!(
                catalog.tool_choice(Some(&ToolChoice::Specific {
                    tool_name: ToolName::new(id)
                })),
                Err(LlmAdapterError::InvalidProviderRequest { .. })
            ));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn collisions_and_provider_unsafe_authored_names_fail_before_transport() {
        let blobs = InMemoryBlobStore::new();
        let schema = blobs
            .put_bytes(br#"{"type":"object"}"#.to_vec())
            .await
            .unwrap();
        for name in ["exec_command", "custom.operation"] {
            let custom = ToolSpec {
                name: ToolName::new(name),
                kind: ToolKind::Function(FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: schema.clone(),
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
                execution: Default::default(),
            };
            assert!(matches!(
                ToolCatalog::resolve(
                    &blobs,
                    &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                    &[builtin("env.run_process"), custom]
                )
                .await,
                Err(LlmAdapterError::InvalidProviderRequest { .. })
            ));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hosted_tools_and_unknown_definitions_cannot_become_client_calls() {
        let blobs = InMemoryBlobStore::new();
        let target = ToolTarget::api_kind(ProviderApiKind::AnthropicMessages);
        let catalog = ToolCatalog::resolve(
            &blobs,
            &target,
            &[builtin("web.fetch"), builtin("web.search")],
        )
        .await
        .expect("hosted catalog");
        assert!(catalog.names.call_ids.is_empty());
        assert!(matches!(
            ToolCatalog::resolve(&blobs, &target, &[builtin("unknown.operation")]).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));
        let invalid = ToolSpec {
            kind: ToolKind::Builtin(BuiltinToolSpec {
                settings: json!({"unknown_option": true}),
            }),
            ..builtin("env.run_process")
        };
        assert!(matches!(
            ToolCatalog::resolve(&blobs, &target, &[invalid]).await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_native_names_return_request_errors() {
        let blobs = InMemoryBlobStore::new();
        let native_tool_ref = blobs
            .put_bytes(br#"{"type":"custom","name":"bad/name"}"#.to_vec())
            .await
            .unwrap();
        let native = ToolSpec {
            name: ToolName::new("custom"),
            kind: ToolKind::ProviderNative(engine::ProviderNativeToolSpec {
                api_kind: ProviderApiKind::OpenAiResponses,
                native_tool_ref,
                execution: ProviderNativeToolExecution::ClientEffect,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            execution: Default::default(),
        };
        assert!(matches!(
            ToolCatalog::resolve(
                &blobs,
                &ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                &[native]
            )
            .await,
            Err(LlmAdapterError::InvalidProviderRequest { .. })
        ));
    }
}
