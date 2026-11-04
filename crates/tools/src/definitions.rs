//! Code-owned definitions resolved from admitted logical identities.

use harness::{
    BuiltinToolSpec, ProviderApiKind, ToolExecutionSpec, ToolId, ToolKind, ToolName,
    ToolParallelism, ToolSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    builtin::{BuiltinTool, BuiltinToolOperation},
    concurrency::{ConcurrencyToolsetConfig, concurrency_tool_definitions},
    environment::control::environment_control_tool_definitions,
    error::{ToolError, ToolResult},
    runtime::{FunctionDefinition, ToolBinding, ToolTarget},
    subagents::{SubagentToolKind, subagent_tool_definition},
    toolset::BuiltinToolPresentation,
    web::{
        fetch::{anthropic_messages_web_fetch_definition, web_fetch_definition},
        search::{
            OpenAiResponsesWebSearchConfig, WebSearchMode, WebSearchToolConfig,
            anthropic_messages_web_search_definition,
        },
    },
};

/// Describe the value actually serialized by an owned result DTO, including
/// output-only required fields and omitted optional values.
pub fn output_schema_for<T: schemars::JsonSchema>() -> Value {
    schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinSettings {
    #[serde(skip_serializing_if = "is_default")]
    pub presentation: BuiltinToolPresentation,
    #[serde(skip_serializing_if = "is_default")]
    pub one_shot: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub unscoped_paths: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub allowed_domains: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub blocked_domains: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub mcp_server_index: String,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Definition {
    Function(FunctionDefinition),
    Native(Value),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBuiltin {
    pub name: ToolName,
    pub definition: Definition,
    pub binding: Option<ToolBinding>,
}

pub fn register(
    id: impl Into<String>,
    settings: BuiltinSettings,
    parallelism: ToolParallelism,
    execution: ToolExecutionSpec,
) -> ToolSpec {
    ToolSpec {
        id: ToolId::new(id),
        kind: ToolKind::Builtin(BuiltinToolSpec {
            settings: serde_json::to_value(settings).expect("built-in settings are serializable"),
        }),
        parallelism,
        execution,
    }
}

pub fn resolve(
    id: &ToolId,
    spec: &BuiltinToolSpec,
    target: &ToolTarget,
) -> ToolResult<Vec<ResolvedBuiltin>> {
    let settings = settings_from_spec(id, spec)?;
    if let Some(tool) = BuiltinTool::from_logical_id(id.as_str()) {
        let prefers_patches = settings.presentation.prefers_patches(target);
        if prefers_patches && tool.operation() == BuiltinToolOperation::EditFile {
            return Ok(Vec::new());
        }
        if settings.presentation == BuiltinToolPresentation::ProviderDefault
            && target.api_kind == ProviderApiKind::OpenAiCompletions
            && tool.operation() == BuiltinToolOperation::ApplyPatch
        {
            return Ok(Vec::new());
        }
        let tool = tool
            .with_surface(settings.presentation.surface(target))
            .with_one_shot(settings.one_shot);
        let mut resolved = Vec::new();
        for variant in tool.variants() {
            let mut definition = match variant.definition(target, !settings.unscoped_paths) {
                Ok(definition) => definition,
                Err(ToolError::UnsupportedCapability { .. })
                    if settings.presentation == BuiltinToolPresentation::ProviderDefault =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if prefers_patches && variant.operation() == BuiltinToolOperation::WriteFile {
                let patch_name = if variant.logical_id().starts_with("vfs.") {
                    "vfs_apply_patch"
                } else {
                    "apply_patch"
                };
                definition.description.get_or_insert_default().push_str(&format!(
                    " Prefer {patch_name} for ordinary text edits. Use this tool for whole-file writes, including binary content and blob transfers."
                ));
            }
            resolved.push(ResolvedBuiltin {
                name: definition.name.clone(),
                definition: Definition::Function(definition),
                binding: Some(variant.binding(target)),
            });
        }
        return Ok(resolved);
    }

    let definition = match id.as_str() {
        "web.search" => {
            let config =
                WebSearchToolConfig::new(settings.allowed_domains, settings.blocked_domains);
            let native = match target.api_kind {
                ProviderApiKind::OpenAiResponses => OpenAiResponsesWebSearchConfig {
                    mode: WebSearchMode::Cached,
                    allowed_domains: config.allowed_domains,
                    blocked_domains: config.blocked_domains,
                    include_sources: true,
                    ..Default::default()
                }
                .native_tool_json()?
                .expect("enabled search"),
                ProviderApiKind::AnthropicMessages => {
                    anthropic_messages_web_search_definition(&config)?
                }
                _ => {
                    return Err(ToolError::UnsupportedCapability {
                        message: "web.search requires OpenAI Responses or Anthropic Messages"
                            .to_owned(),
                    });
                }
            };
            return Ok(vec![ResolvedBuiltin {
                name: ToolName::new("web_search"),
                definition: Definition::Native(native),
                binding: None,
            }]);
        }
        "web.fetch" if target.api_kind == ProviderApiKind::AnthropicMessages => {
            return Ok(vec![ResolvedBuiltin {
                name: ToolName::new("web_fetch"),
                definition: Definition::Native(anthropic_messages_web_fetch_definition()),
                binding: None,
            }]);
        }
        "web.fetch" => web_fetch_definition(),
        name if crate::blobs::BlobTool::from_logical_id(name).is_some() => {
            crate::blobs::BlobTool::from_logical_id(name)
                .expect("blob tool identity matched")
                .definition()?
        }
        "code.execute" => crate::code::code_execute_tool_definition()?,
        "subagent.run" => subagent_tool_definition(SubagentToolKind::Run)?,
        "subagent.spawn" => subagent_tool_definition(SubagentToolKind::Spawn)?,
        harness::MCP_FIND_TOOLS_TOOL_ID => FunctionDefinition::new(
            "mcp_find_tools",
            format!(
                "Browse, search, or load full definitions for live MCP tools. Browse with no query or names; search with query (tool names, descriptions, and argument names are indexed); load full definitions with server plus up to five names. Browse and search are byte-paged and may truncate oversized hits; use server plus names when a hit asks for its full definition. Available servers: {}",
                settings.mcp_server_index
            ),
            json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "query": {"type": "string"},
                    "names": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 5},
                    "cursor": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            }),
        ),
        harness::MCP_CALL_TOOL_ID => FunctionDefinition::new(
            "mcp_call",
            format!(
                "Call a tool found on a live MCP server. Available servers: {}",
                settings.mcp_server_index
            ),
            json!({
                "type": "object",
                "properties": {"server": {"type": "string"}, "tool": {"type": "string"}, "arguments": {"type": "object"}},
                "required": ["server", "tool", "arguments"],
                "additionalProperties": false
            }),
        ),
        name if name.starts_with("concurrency.") => {
            concurrency_tool_definitions(&ConcurrencyToolsetConfig {
                enabled: true,
                timer: true,
            })?
            .into_iter()
            .find(|definition| format!("concurrency.{}", definition.name) == name)
            .ok_or_else(|| unknown(id))?
        }
        name if name.starts_with("environment.") => environment_control_tool_definitions(true)?
            .into_iter()
            .find(|definition| {
                format!(
                    "environment.{}",
                    definition.name.as_str().trim_start_matches("environment_")
                ) == name
            })
            .ok_or_else(|| unknown(id))?,
        _ => return Err(unknown(id)),
    };
    Ok(vec![ResolvedBuiltin {
        name: definition.name.clone(),
        binding: Some(ToolBinding::new(definition.name.clone(), id.as_str())),
        definition: Definition::Function(definition),
    }])
}

fn settings_from_spec(id: &ToolId, spec: &BuiltinToolSpec) -> ToolResult<BuiltinSettings> {
    if spec.settings.is_null() {
        Ok(BuiltinSettings::default())
    } else {
        serde_json::from_value(spec.settings.clone()).map_err(|error| ToolError::InvalidRequest {
            message: format!("invalid settings for built-in {id}: {error}"),
        })
    }
}

/// Resolve an already-admitted call using its original exposed name. New
/// provider responses must still resolve identities against the advertised
/// catalog; execution must not make historical names available to new calls.
pub(crate) fn resolve_call_binding(
    id: &ToolId,
    spec: &BuiltinToolSpec,
    target: &ToolTarget,
    exposed_name: &ToolName,
) -> ToolResult<ToolBinding> {
    if let Some(binding) = resolve(id, spec, target)?
        .into_iter()
        .find(|tool| &tool.name == exposed_name)
        .and_then(|tool| tool.binding)
    {
        return Ok(binding);
    }

    let settings = settings_from_spec(id, spec)?;
    // Calls admitted before patch-oriented presentations stopped advertising
    // exact replacement must still execute with their original argument shape.
    if settings.presentation.prefers_patches(target)
        && let Some(tool) = BuiltinTool::from_logical_id(id.as_str())
        && tool.operation() == BuiltinToolOperation::EditFile
    {
        let tool = tool.with_surface(settings.presentation.surface(target));
        if tool.name_str() == exposed_name.as_str() {
            return Ok(tool.with_one_shot(settings.one_shot).binding(target));
        }
    }
    // Completions previously advertised canonical process and patch tools.
    // Retried or parked calls retain their admitted identity, original name,
    // settings, and model, including the one-shot execution restriction.
    if target.api_kind == ProviderApiKind::OpenAiCompletions
        && settings.presentation == BuiltinToolPresentation::ProviderDefault
        && let Some(tool) = BuiltinTool::from_logical_id(id.as_str())
        && matches!(
            tool.operation(),
            BuiltinToolOperation::RunProcess
                | BuiltinToolOperation::ContinueProcess
                | BuiltinToolOperation::ApplyPatch
        )
        && tool.name_str() == exposed_name.as_str()
    {
        return Ok(tool.with_one_shot(settings.one_shot).binding(target));
    }

    Err(ToolError::UnsupportedCapability {
        message: format!("unknown tool: {exposed_name}"),
    })
}

fn unknown(id: &ToolId) -> ToolError {
    ToolError::UnsupportedCapability {
        message: format!("unknown built-in tool {id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_exact_replacements_keep_their_binding_without_readvertising() {
        for (api, presentation) in [
            (
                ProviderApiKind::OpenAiResponses,
                BuiltinToolPresentation::ProviderDefault,
            ),
            (
                ProviderApiKind::OpenAiResponses,
                BuiltinToolPresentation::CodexLike,
            ),
            (
                ProviderApiKind::OpenAiCompletions,
                BuiltinToolPresentation::CodexLike,
            ),
        ] {
            let target = ToolTarget::api_kind(api);
            let spec = BuiltinToolSpec {
                settings: serde_json::to_value(BuiltinSettings {
                    presentation,
                    ..Default::default()
                })
                .unwrap(),
            };
            for (id, name) in [
                ("env.edit_file", "edit_file"),
                ("vfs.edit_file", "vfs_edit_file"),
            ] {
                let id = ToolId::new(id);
                let name = ToolName::new(name);
                assert!(resolve(&id, &spec, &target).unwrap().is_empty());
                let binding = resolve_call_binding(&id, &spec, &target, &name).unwrap();
                assert_eq!(binding.logical_id, id.as_str());
                assert_eq!(binding.tool_name, name);
                assert_eq!(binding.adapter_id.as_deref(), Some("codex"));
                assert!(
                    resolve_call_binding(&id, &spec, &target, &ToolName::new("apply_patch"))
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn recorded_completions_calls_keep_only_their_matching_historical_binding() {
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiCompletions);
        let spec = BuiltinToolSpec {
            settings: json!({}),
        };
        for (id, name) in [
            ("env.run_process", "run_process"),
            ("env.continue_process", "continue_process"),
            ("env.apply_patch", "apply_patch"),
            ("vfs.apply_patch", "vfs_apply_patch"),
        ] {
            let id = ToolId::new(id);
            let name = ToolName::new(name);
            let binding = resolve_call_binding(&id, &spec, &target, &name).unwrap();
            assert_eq!(binding.logical_id, id.as_str());
            assert_eq!(binding.tool_name, name);
            assert_eq!(binding.adapter_id.as_deref(), Some("canonical"));
            assert!(
                resolve(&id, &spec, &target)
                    .unwrap()
                    .iter()
                    .all(|tool| tool.name != name)
            );
        }
        for (id, name) in [
            ("env.run_process", "Bash"),
            ("env.run_process", "continue_process"),
            ("env.apply_patch", "vfs_apply_patch"),
            ("vfs.apply_patch", "apply_patch"),
            ("env.edit_file", "apply_patch"),
        ] {
            assert!(matches!(
                resolve_call_binding(&ToolId::new(id), &spec, &target, &ToolName::new(name)),
                Err(ToolError::UnsupportedCapability { .. })
            ));
        }
        for (target, settings) in [
            (target, json!({"presentation":"codex_like"})),
            (
                ToolTarget::api_kind(ProviderApiKind::OpenAiResponses),
                json!({}),
            ),
            (
                ToolTarget::api_kind(ProviderApiKind::AnthropicMessages),
                json!({}),
            ),
        ] {
            assert!(matches!(
                resolve_call_binding(
                    &ToolId::new("env.run_process"),
                    &BuiltinToolSpec { settings },
                    &target,
                    &ToolName::new("run_process")
                ),
                Err(ToolError::UnsupportedCapability { .. })
            ));
        }
    }

    #[test]
    fn registrations_store_only_logical_operations_and_contract_options() {
        let mut config = crate::toolset::ToolsetConfig::workspace();
        config.builtin.environment = crate::toolset::EnvironmentToolsetConfig::basic();
        config.builtin.environment.continue_process = false;
        let registered = crate::toolset::register_toolset(&config).expect("registration");
        assert!(
            registered
                .tools
                .contains_key(&ToolId::new("env.run_process"))
        );
        assert!(
            !registered
                .tools
                .contains_key(&ToolId::new("env.continue_process"))
        );
        assert!(!registered.tools.contains_key(&ToolId::new("exec_command")));
        for tool in registered.tools.values() {
            let ToolKind::Builtin(spec) = &tool.kind else {
                panic!("code-owned registration");
            };
            assert_eq!(
                spec.settings,
                if tool.id.as_str() == "env.run_process" {
                    json!({"one_shot": true})
                } else {
                    json!({})
                }
            );
        }
    }
}
