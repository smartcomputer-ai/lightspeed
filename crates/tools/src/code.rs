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
        "Run a JavaScript async function body once. Call the session's granted tools with await tools.tool_name(arguments), or tools[\"tool_name\"](arguments); tool arguments and successful JSON values follow the ordinary tool contracts. Promise.all, Promise.allSettled, loops, and dependent calls are supported. Use text(value) for selected JSON output, await media(source, options) to show a supported image/PDF to the model, or await file(source, options) for a downloadable file attachment/link. Both helpers accept a full content reference, recorded media:/file: handle, tool-result descriptor, or explicit inline {text:...}, {json:...}, or {bytes:[0..255]}; a string always means a reference, never literal text or a path. Options are optional descriptive name/media_type metadata. Helpers return the admitted descriptor. They call the ordinary blob tools: media requires blob_read, file requires blob_info, and inline input additionally requires blob_put. Those calls use the same allowlist, call counts, and byte limits as tools calls. Only explicitly selected assets enter the outer result; text objects and return values never attach media/files. Await helpers to retain their output, and return a JSON value separately. Failed calls reject with kind, message, and optional value. There is no filesystem, network, module loading, TypeScript, or recursive code_execute; external effects must use tools. Unawaited calls are cancelled when the script finishes. Failure reports retain completed tool outcomes and earlier selected output; completed side effects are not rolled back and the whole script is not retried.",
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
    let mut attachment_schema = crate::definitions::output_schema_for::<harness::Attachment>();
    let definitions = attachment_schema.as_object_mut().unwrap().remove("$defs");
    attachment_schema.as_object_mut().unwrap().remove("$schema");
    let mut schema = json!({
        "type": "object",
        "properties": {
            "status": {"enum": ["succeeded", "failed", "cancelled", "interrupted"]},
            "output_available": {"type": "boolean", "description": "Whether a JavaScript receipt was retained. False means selected output may be missing after interruption, even when tool outcomes are known."},
            "output": {"type": "array", "items": {}, "description": "Selected output in emission order: raw text(value) JSON values and media/file attachment descriptors as {kind, data}. Only the top-level attachments list selects native media or file links; a text value with the same shape remains ordinary JSON."},
            "attachments": {"type": "array", "items": attachment_schema, "description": "Validated media/file selections. Media is supplied as companion model input; files are downloadable attachment metadata."},
            "output_errors": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "selection_index": {"type":"integer", "minimum":0},
                        "kind": {"enum":["text", "media", "file"]},
                        "request_id": {"type":"string"},
                        "message": {"type":"string"}
                    },
                    "required":["selection_index", "kind", "message"]
                },
                "description": "Output finalization errors, if any. Valid sibling selections remain available; inspect these before claiming an asset was delivered."
            },
            "return_value": {"description": "Final returned JSON value, or null."},
            "error": {"type": ["object", "null"], "description": "Script error kind and message, if any."},
            "interruption": {"description": "Workflow interruption when execution could not return normally."},
            "cleanup_error": {"type": ["string", "null"]},
            "report_unavailable": {"type": ["string", "null"]},
            "calls": {"type": "object", "description": "Counts of canonical per-tool outcomes by status."},
            "report_ref": {"type": "string", "description": "Artifact reference to detailed execution and canonical per-tool outcomes."}
        },
        "required": ["status", "output_available", "output", "return_value", "error", "report_ref"]
    });
    if let Some(definitions) = definitions {
        schema["$defs"] = definitions;
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_schema_accepts_legacy_text_and_typed_media_file_selections() {
        let validator = jsonschema::validator_for(&code_execution_output_schema()).unwrap();
        let reference = BlobRef::from_bytes(b"content");
        let mut report = json!({
            "status":"succeeded", "output_available":true,
            "output":[{"answer":42}], "return_value":null,
            "error":null, "report_ref":reference
        });
        validator.validate(&report).unwrap();
        let media = harness::Attachment::Media(
            harness::media::MediaDescriptor::new(reference.clone(), "image/png", Some("plot.png"))
                .unwrap(),
        );
        let file = harness::Attachment::File(harness::FileAttachment::new(
            reference,
            "report.txt".into(),
            Some("text/plain".into()),
        ));
        report["output"].as_array_mut().unwrap().extend([
            serde_json::to_value(&media).unwrap(),
            serde_json::to_value(&file).unwrap(),
        ]);
        report["attachments"] = json!([media, file]);
        report["output_errors"] = json!([{
            "selection_index":3, "kind":"media", "request_id":"call-4", "message":"unavailable"
        }]);
        validator.validate(&report).unwrap();
        report["attachments"][0]["data"]["content_ref"] = json!(false);
        assert!(!validator.is_valid(&report));
    }

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
