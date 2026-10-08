//! Durable inputs for an ephemeral code execution.
//!
//! These types contain content references and admitted budgets, never an
//! interpreter context, materialized source, or a copy of session authority.
//! The runtime loads the referenced artifacts before entering the interpreter.

use std::fmt;

use harness::{BlobRef, BlobRefError};
use serde::{Deserialize, Serialize};

/// The stable workflow type used by the ordinary start-on-call tool recipe.
pub const CODE_EXECUTION_WORKFLOW_TYPE: &str = "CodeExecutionWorkflow";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodePrepareActivityRequest {
    pub start: crate::WorkflowToolStartArgs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodePrepareActivityResult {
    Prepared { descriptor: CodeExecutionDescriptor },
    Rejected { error_ref: BlobRef },
}

/// Only the reference crosses the activity boundary. Script output and the
/// complete code tool call report remain in the parent universe's CAS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeRunActivityResult {
    pub report_ref: BlobRef,
    pub succeeded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeExecutionInterruption {
    HolderCancelled,
    WorkflowCancelled,
    PreparationFailed,
    ActivityFailed,
    ActivityTimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodeExecutionTerminal {
    Completed {
        result: CodeRunActivityResult,
    },
    Rejected {
        error_ref: BlobRef,
    },
    Interrupted {
        reason: CodeExecutionInterruption,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<CodeRunActivityResult>,
    },
}

/// Finalization is safe to repeat. It closes the stable scope even when a
/// prepare or runner activity ended without delivering its receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeFinalizeActivityRequest {
    pub start: crate::WorkflowToolStartArgs,
    pub descriptor: Option<CodeExecutionDescriptor>,
    pub terminal: CodeExecutionTerminal,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CodeExecutionPhase {
    #[default]
    Starting,
    Preparing,
    Running,
    Finalizing,
    Resolved,
    Cancelled,
}

/// Queryable durable orchestration state; no JavaScript heap or raw outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeExecutionSnapshot {
    pub phase: CodeExecutionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<CodeExecutionDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<CodeExecutionTerminal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<harness::PromiseResolution>,
}

/// Small, immutable input shared by code-execution orchestration and its runner.
///
/// The parent session owns the execution scope and its bindings. Possessing this
/// descriptor does not authorize tool calls. Validate it before loading artifacts
/// or admitting an execution; deserialization alone is not validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeExecutionDescriptor {
    /// Stable identity of the session-owned execution scope.
    pub execution_id: String,
    /// Parent session's composed workflow id, without a Temporal run id so the
    /// identity remains stable across the parent's Continue-as-New transitions.
    pub session_workflow_id: String,
    /// UTF-8 JavaScript source in the parent session's universe-scoped CAS.
    pub source_ref: BlobRef,
    /// Immutable matched callable specifications and bindings in the same CAS.
    /// This catalog is metadata, not an independent authorization database.
    pub catalog_ref: BlobRef,
    pub limits: CodeExecutionLimits,
}

impl CodeExecutionDescriptor {
    /// Validate structure only. Session admission must separately establish
    /// ownership, grants, current bindings, and the permitted execution budgets.
    pub fn validate(&self) -> Result<(), CodeExecutionValidationError> {
        if self.execution_id.trim().is_empty() {
            return Err(CodeExecutionValidationError::EmptyExecutionId);
        }
        if crate::split_workflow_id(&self.session_workflow_id).is_none_or(
            |(universe_id, session_id)| {
                crate::compose_workflow_id(universe_id, &session_id) != self.session_workflow_id
            },
        ) {
            return Err(CodeExecutionValidationError::InvalidSessionWorkflowId);
        }
        for (field, reference) in [
            ("source_ref", &self.source_ref),
            ("catalog_ref", &self.catalog_ref),
        ] {
            BlobRef::parse(reference.as_str()).map_err(|source| {
                CodeExecutionValidationError::InvalidBlobReference { field, source }
            })?;
        }
        self.limits.validate()
    }
}

/// Explicit, positive budgets admitted for a single execution attempt.
///
/// No deployment defaults are implied. User-requested options may only narrow
/// these budgets. Engine limits must be enforced by the runner; call and payload
/// limits must also be enforced at the trusted session/bridge boundary. These
/// bounds are not containment of native interpreter memory faults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeExecutionLimits {
    /// Total attempt time, including input loading, interpreter capacity waits,
    /// JavaScript evaluation, and time awaiting host calls.
    #[schemars(range(min = 1))]
    pub timeout_ms: u64,
    /// Interpreter-managed heap allocation budget.
    #[schemars(range(min = 1))]
    pub max_memory_bytes: u64,
    /// Interpreter-managed native stack budget, separate from its heap budget.
    #[schemars(range(min = 1))]
    pub max_stack_bytes: u64,
    #[schemars(range(min = 1))]
    pub max_source_bytes: u64,
    #[schemars(range(min = 1))]
    pub max_catalog_bytes: u64,
    /// Serialized JSON bytes in one guest tool request, before dispatch.
    #[schemars(range(min = 1))]
    pub max_request_bytes: u64,
    /// Serialized JSON bytes in one host completion, before entering the guest.
    /// Larger tool outputs require authorized artifact handles.
    #[schemars(range(min = 1))]
    pub max_result_bytes: u64,
    /// Total serialized output bytes retained for the final script report.
    #[schemars(range(min = 1))]
    pub max_output_bytes: u64,
    #[schemars(range(min = 1))]
    pub max_tool_calls: u32,
    /// Outstanding bridge requests, including durable-promise waits. This does
    /// not change the ordinary tool's own scheduling or concurrency policies.
    #[schemars(range(min = 1))]
    pub max_outstanding_tool_calls: u32,
}

impl CodeExecutionLimits {
    pub fn validate(&self) -> Result<(), CodeExecutionValidationError> {
        for (field, value) in [
            ("timeout_ms", self.timeout_ms),
            ("max_memory_bytes", self.max_memory_bytes),
            ("max_stack_bytes", self.max_stack_bytes),
            ("max_source_bytes", self.max_source_bytes),
            ("max_catalog_bytes", self.max_catalog_bytes),
            ("max_request_bytes", self.max_request_bytes),
            ("max_result_bytes", self.max_result_bytes),
            ("max_output_bytes", self.max_output_bytes),
            ("max_tool_calls", u64::from(self.max_tool_calls)),
            (
                "max_outstanding_tool_calls",
                u64::from(self.max_outstanding_tool_calls),
            ),
        ] {
            if value == 0 {
                return Err(CodeExecutionValidationError::ZeroLimit { field });
            }
        }
        if self.max_outstanding_tool_calls > self.max_tool_calls {
            return Err(CodeExecutionValidationError::OutstandingCallsExceedTotal {
                outstanding: self.max_outstanding_tool_calls,
                total: self.max_tool_calls,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeExecutionValidationError {
    EmptyExecutionId,
    InvalidSessionWorkflowId,
    InvalidBlobReference {
        field: &'static str,
        source: BlobRefError,
    },
    ZeroLimit {
        field: &'static str,
    },
    OutstandingCallsExceedTotal {
        outstanding: u32,
        total: u32,
    },
}

impl fmt::Display for CodeExecutionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyExecutionId => formatter.write_str("execution_id must not be empty"),
            Self::InvalidSessionWorkflowId => {
                formatter.write_str("session_workflow_id must identify a parent session")
            }
            Self::InvalidBlobReference { field, source } => write!(formatter, "{field}: {source}"),
            Self::ZeroLimit { field } => write!(formatter, "{field} must be greater than zero"),
            Self::OutstandingCallsExceedTotal { outstanding, total } => write!(
                formatter,
                "max_outstanding_tool_calls ({outstanding}) exceeds max_tool_calls ({total})"
            ),
        }
    }
}

impl std::error::Error for CodeExecutionValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBlobReference { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn descriptor() -> CodeExecutionDescriptor {
        CodeExecutionDescriptor {
            execution_id: "code:execution-1".to_owned(),
            session_workflow_id: crate::compose_workflow_id(
                uuid::Uuid::from_u128(1),
                &harness::SessionId::new("parent-session"),
            ),
            source_ref: BlobRef::from_bytes(b"return await tools.read({path: 'file'});"),
            catalog_ref: BlobRef::from_bytes(b"[]"),
            limits: CodeExecutionLimits {
                timeout_ms: 10_000,
                max_memory_bytes: 16 * 1024 * 1024,
                max_stack_bytes: 256 * 1024,
                max_source_bytes: 64 * 1024,
                max_catalog_bytes: 256 * 1024,
                max_request_bytes: 64 * 1024,
                max_result_bytes: 256 * 1024,
                max_output_bytes: 64 * 1024,
                max_tool_calls: 100,
                max_outstanding_tool_calls: 8,
            },
        }
    }

    #[test]
    fn descriptor_round_trips_with_references_instead_of_materialized_inputs() {
        let expected = descriptor();
        expected.validate().expect("valid descriptor");
        let value = serde_json::to_value(&expected).expect("serialize descriptor");
        assert_eq!(value["source_ref"], expected.source_ref.as_str());
        assert_eq!(value["catalog_ref"], expected.catalog_ref.as_str());
        assert_eq!(value.as_object().expect("object").len(), 5);
        let decoded: CodeExecutionDescriptor =
            serde_json::from_value(value).expect("deserialize descriptor");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn descriptor_rejects_unknown_fields_and_has_no_implicit_limits() {
        let mut value = serde_json::to_value(descriptor()).expect("serialize descriptor");
        value["source"] = json!("return 42;");
        assert!(serde_json::from_value::<CodeExecutionDescriptor>(value).is_err());

        let mut value = serde_json::to_value(descriptor()).expect("serialize descriptor");
        value["limits"]
            .as_object_mut()
            .expect("limits")
            .remove("timeout_ms");
        assert!(serde_json::from_value::<CodeExecutionDescriptor>(value).is_err());
    }

    #[test]
    fn validation_rejects_invalid_execution_and_parent_identities() {
        let mut input = descriptor();
        input.execution_id = "  ".to_owned();
        assert_eq!(
            input.validate(),
            Err(CodeExecutionValidationError::EmptyExecutionId)
        );
        input = descriptor();
        for invalid in [
            "parent-session",
            "not-a-uuid/parent-session",
            "00000000-0000-0000-0000-000000000001/bad/session",
            "00000000000000000000000000000001/parent-session",
        ] {
            input.session_workflow_id = invalid.to_owned();
            assert_eq!(
                input.validate(),
                Err(CodeExecutionValidationError::InvalidSessionWorkflowId)
            );
        }
    }

    #[test]
    fn validation_rejects_malformed_cas_refs_accepted_by_blob_deserialization() {
        for field in ["source_ref", "catalog_ref"] {
            let mut value = serde_json::to_value(descriptor()).expect("serialize descriptor");
            value[field] = json!("sha256:not-a-digest");
            let decoded: CodeExecutionDescriptor =
                serde_json::from_value(value).expect("blob deserialization is structural");
            assert_eq!(
                decoded.validate(),
                Err(CodeExecutionValidationError::InvalidBlobReference {
                    field,
                    source: BlobRefError::InvalidFormat {
                        value: "sha256:not-a-digest".to_owned()
                    },
                }),
            );
        }
    }

    #[test]
    fn every_limit_is_positive_and_outstanding_calls_cannot_exceed_total() {
        let value = serde_json::to_value(descriptor().limits).expect("serialize limits");
        for field in value.as_object().expect("limits object").keys() {
            let mut invalid = value.clone();
            invalid[field] = json!(0);
            let limits: CodeExecutionLimits =
                serde_json::from_value(invalid).expect("numeric limits deserialize");
            assert!(
                matches!(limits.validate(), Err(CodeExecutionValidationError::ZeroLimit { field: actual }) if actual == field)
            );
        }
        let mut limits = descriptor().limits;
        limits.max_outstanding_tool_calls = limits.max_tool_calls + 1;
        assert_eq!(
            limits.validate(),
            Err(CodeExecutionValidationError::OutstandingCallsExceedTotal {
                outstanding: 101,
                total: 100
            }),
        );
    }
}
