//! CoreAgent runtime I/O traits and request/result records.
//!
//! These traits are specific to CoreAgent; the lower-level session kernel
//! should not impose this I/O shape on custom agents.
//!
//! `LlmGenerationRequest`, `LlmGenerationResult`,
//! `ToolInvocationBatchRequest`, and `ToolInvocationBatchResult` are shared
//! serializable records used by both local and workflow substrates. The
//! `CoreAgentLlm` and `CoreAgentTools` traits are execution adapter traits for
//! local runtimes, tests, and workflow activities. Workflow code that cannot
//! hold `Send + Sync` async adapters should fulfill `CoreAgentAction` values
//! directly instead of implementing these traits inside the workflow.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    BlobRef, ContextCompactionRequest, ContextCompactionResult, ContextEntryInput,
    LlmGenerationFacts, LlmGenerationStatus, LlmRequest, RunId, SessionId, ToolBatchId,
    ToolBatchResumeDirective, ToolCallId, ToolCallStatus, ToolExecutionTarget, ToolName, TurnId,
};

#[async_trait]
pub trait CoreAgentLlm: Send + Sync {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError>;

    async fn compact_context(
        &self,
        request: ContextCompactionRequest,
    ) -> Result<ContextCompactionResult, CoreAgentIoError> {
        let _ = request;
        Err(CoreAgentIoError::Failed {
            message: "context compaction runtime unavailable".to_owned(),
        })
    }
}

#[async_trait]
pub trait CoreAgentTools: Send + Sync {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGenerationRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub request: LlmRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGenerationResult {
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub status: LlmGenerationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_ref: Option<BlobRef>,
    pub context_entries: Vec<ContextEntryInput>,
    pub facts: LlmGenerationFacts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationBatchRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_targets: BTreeMap<String, ToolExecutionTarget>,
    pub calls: Vec<ToolInvocationRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationRequest {
    pub call_id: ToolCallId,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
    pub execution_target: Option<ToolExecutionTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationBatchResult {
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub results: Vec<ToolInvocationResult>,
}

impl ToolInvocationBatchResult {
    pub fn single_result(self) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let mut results = self.results;
        if results.len() != 1 {
            return Err(CoreAgentIoError::Failed {
                message: format!(
                    "expected exactly one tool invocation result, got {}",
                    results.len()
                ),
            });
        }
        Ok(results.remove(0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolBatchOutcome {
    Completed {
        result: ToolInvocationBatchResult,
    },
    Deferred {
        batch_id: ToolBatchId,
        resume_directive: ToolBatchResumeDirective,
    },
}

impl ToolBatchOutcome {
    pub fn completed(result: ToolInvocationBatchResult) -> Self {
        Self::Completed { result }
    }

    pub fn completed_result(self) -> Result<ToolInvocationBatchResult, CoreAgentIoError> {
        match self {
            Self::Completed { result } => Ok(result),
            Self::Deferred { batch_id, .. } => Err(CoreAgentIoError::Failed {
                message: format!("tool batch {batch_id} deferred instead of completing"),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolEffect {
    pub kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationResult {
    pub call_id: ToolCallId,
    pub status: ToolCallStatus,
    pub output_ref: Option<BlobRef>,
    pub model_visible_output_ref: Option<BlobRef>,
    pub error_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffect>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CoreAgentIoError {
    #[error("core agent I/O failed: {message}")]
    Failed { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_batch_single_result_requires_exactly_one_result() {
        let empty = ToolInvocationBatchResult {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            results: Vec::new(),
        };
        assert!(empty.single_result().is_err());
    }
}
