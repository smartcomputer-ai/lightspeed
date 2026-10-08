//! Model contract and admission metadata for composed JavaScript tool calls.

use std::collections::BTreeSet;

use harness::{BlobRef, CodeModeLimits, ToolName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ToolError, ToolResult};

pub const CODE_EXECUTE_TOOL_NAME: &str = "code_execute";
pub const CODE_EXECUTE_WORKFLOW_TOOL_ID: &str = "code-execute";
pub const CODE_EXECUTE_WORKFLOW_SEMANTIC_TYPE: &str = "lightspeed.code.execute.v1";
pub const CODE_EXECUTION_WORKFLOW_TYPE: &str = "CodeExecutionWorkflow";
pub const CODE_EXECUTION_OVERHEAD_MS: u64 = 240_000;
pub const CODE_EXECUTION_DEADLINE_CEILING_MS: u64 =
    harness::CODE_MODE_TIMEOUT_CEILING_MS + CODE_EXECUTION_OVERHEAD_MS;

pub fn is_code_execution_binding(tool_id: &str, semantic_type: &str) -> bool {
    tool_id == CODE_EXECUTE_WORKFLOW_TOOL_ID && semantic_type == CODE_EXECUTE_WORKFLOW_SEMANTIC_TYPE
}

/// Generated JavaScript is an async function body. Options only narrow the
/// session grant; routing identities and artifact references are host-owned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeExecuteArgs {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl CodeExecuteArgs {
    pub fn effective_limits(&self, granted: CodeModeLimits) -> ToolResult<CodeModeLimits> {
        granted
            .validate()
            .map_err(|error| ToolError::InvalidRequest {
                message: error.to_string(),
            })?;
        if self.code.trim().is_empty() || self.code.len() as u64 > granted.max_source_bytes {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "code must be nonempty UTF-8 JavaScript of at most {} bytes",
                    granted.max_source_bytes
                ),
            });
        }
        let timeout_ms = self.timeout_ms.unwrap_or(granted.timeout_ms);
        if timeout_ms == 0 || timeout_ms > granted.timeout_ms {
            return Err(ToolError::InvalidRequest {
                message: format!("timeout_ms must be between 1 and {}", granted.timeout_ms),
            });
        }
        Ok(CodeModeLimits {
            timeout_ms,
            ..granted
        })
    }
}

/// Trusted facts pinned before starting the generic workflow invocation.
/// These are never accepted from script arguments and do not replace session
/// admission: the session resolves and authorizes each actual callable tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeExecutionContextV1 {
    pub version: u32,
    pub parent_session_id: String,
    pub parent_run_id: u64,
    pub source_ref: BlobRef,
    pub limits: CodeModeLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<BTreeSet<ToolName>>,
}

impl CodeExecutionContextV1 {
    pub const VERSION: u32 = 1;

    pub fn validate(&self) -> ToolResult<()> {
        if self.version != Self::VERSION || self.parent_session_id.trim().is_empty() {
            return Err(ToolError::InvalidRequest {
                message: "invalid code execution context version or parent identity".to_owned(),
            });
        }
        BlobRef::parse(self.source_ref.as_str()).map_err(|error| ToolError::InvalidRequest {
            message: format!("invalid code execution source reference: {error}"),
        })?;
        self.limits
            .validate()
            .map_err(|error| ToolError::InvalidRequest {
                message: error.to_string(),
            })?;
        if let Some(allowed) = &self.allowed_tools
            && (allowed.len() > 4096
                || allowed.iter().any(|name| {
                    name.as_str().trim().is_empty()
                        || name.as_str().len() > 512
                        || matches!(name.as_str(), CODE_EXECUTE_TOOL_NAME | "code.execute")
                }))
        {
            return Err(ToolError::InvalidRequest {
                message:
                    "invalid code execution tool allowlist; recursive execution is unavailable"
                        .to_owned(),
            });
        }
        Ok(())
    }
}

pub fn code_execute_tool_definition() -> ToolResult<crate::runtime::FunctionDefinition> {
    Ok(crate::runtime::FunctionDefinition::new(
        CODE_EXECUTE_TOOL_NAME,
        "Run a JavaScript async function body once. Call the session's granted tools with await tools.tool_name(arguments), or tools[\"tool_name\"](arguments); tool arguments and successful JSON values follow the ordinary tool contracts. Promise.all, Promise.allSettled, loops, and dependent calls are supported. Use text(value) to retain selected JSON output and return a JSON value. Failed calls reject with kind, message, and optional value. There is no filesystem, network, module loading, TypeScript, or recursive code_execute; external effects must use tools. Unawaited calls are cancelled when the script finishes. Failure reports retain completed tool outcomes; completed side effects are not rolled back and the whole script is not retried.",
        json!({
            "type": "object",
            "properties": {
                "code": {"type": "string", "minLength": 1, "description": "JavaScript async function body; use await and return directly."},
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": harness::CODE_MODE_TIMEOUT_CEILING_MS, "description": "Optional total attempt limit including input loading, interpreter capacity waits, JavaScript evaluation, and tool waits; may only narrow the session's configured limit."}
            },
            "required": ["code"],
            "additionalProperties": false
        }),
    ).with_output_schema(code_execution_output_schema()))
}

/// Compact model output points to a detailed report containing canonical
/// per-call outcomes, so durable history does not inline every tool result.
pub fn code_execution_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {"enum": ["succeeded", "failed", "cancelled", "interrupted"]},
            "output_available": {"type": "boolean", "description": "Whether a JavaScript receipt was retained. False means selected output may be missing after interruption, even when tool outcomes are known."},
            "output": {"type": "array", "items": {}, "description": "Values explicitly emitted with text(value)."},
            "return_value": {"description": "Final returned JSON value, or null."},
            "error": {"type": ["object", "null"], "description": "Script error kind and message, if any."},
            "interruption": {"description": "Workflow interruption when execution could not return normally."},
            "cleanup_error": {"type": ["string", "null"]},
            "report_unavailable": {"type": ["string", "null"]},
            "calls": {"type": "object", "description": "Counts of canonical per-tool outcomes by status."},
            "report_ref": {"type": "string", "description": "Artifact reference to detailed execution and canonical per-tool outcomes."}
        },
        "required": ["status", "output_available", "output", "return_value", "error", "report_ref"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_time_options_cannot_widen_grants() {
        let mut args = CodeExecuteArgs {
            code: "return 1;".into(),
            timeout_ms: Some(20),
        };
        let limits = CodeModeLimits {
            timeout_ms: 30,
            max_source_bytes: 9,
            ..Default::default()
        };
        assert_eq!(args.effective_limits(limits).unwrap().timeout_ms, 20);
        args.timeout_ms = Some(31);
        assert!(matches!(
            args.effective_limits(limits),
            Err(ToolError::InvalidRequest { .. })
        ));
        args.timeout_ms = Some(0);
        assert!(args.effective_limits(limits).is_err());
        args.timeout_ms = None;
        args.code.push(' ');
        assert!(args.effective_limits(limits).is_err());
        assert!(
            serde_json::from_value::<CodeExecuteArgs>(
                json!({"code":"return 1", "source_ref":"forged"})
            )
            .is_err()
        );
    }

    #[test]
    fn empty_allowlist_is_valid_and_recursive_allowlist_is_rejected() {
        let mut context = CodeExecutionContextV1 {
            version: 1,
            parent_session_id: "session".into(),
            parent_run_id: 1,
            source_ref: BlobRef::from_bytes(b"return 1"),
            limits: Default::default(),
            allowed_tools: Some(BTreeSet::new()),
        };
        context.validate().unwrap();
        context
            .allowed_tools
            .as_mut()
            .unwrap()
            .insert(ToolName::new(CODE_EXECUTE_TOOL_NAME));
        assert!(context.validate().is_err());
    }
}
