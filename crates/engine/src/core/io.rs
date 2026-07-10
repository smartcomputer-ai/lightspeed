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
    AwaitSpec, BlobRef, ContextCompactionRequest, ContextCompactionResult, ContextEntryInput,
    ContextEntryKind, LlmGenerationFacts, LlmGenerationStatus, LlmRequest, PromiseSource, RunId,
    SessionId, ToolBatchId, ToolCallId, ToolCallStatus, ToolExecutionTarget, ToolName, TurnId,
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

    async fn check_promise_source(
        &self,
        request: PromiseSourceCheckRequest,
    ) -> Result<PromiseSourceCheckResult, CoreAgentIoError> {
        let _ = request;
        Ok(PromiseSourceCheckResult::Pending)
    }

    async fn cancel_promise_source(
        &self,
        request: PromiseSourceCancelRequest,
    ) -> Result<PromiseSourceCancelResult, CoreAgentIoError> {
        let _ = request;
        Ok(PromiseSourceCancelResult { cancelled: false })
    }

    async fn subscribe_promise_source(
        &self,
        request: PromiseSourceSubscribeRequest,
    ) -> Result<PromiseSourceCheckResult, CoreAgentIoError> {
        let _ = request;
        Ok(PromiseSourceCheckResult::Pending)
    }
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
pub struct PromiseSourceCheckRequest {
    pub source: PromiseSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PromiseSourceCheckResult {
    Pending,
    Resolved { payload_ref: Option<BlobRef> },
    Failed { error_ref: Option<BlobRef> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourceCancelRequest {
    pub source: PromiseSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourceCancelResult {
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourceSubscribeRequest {
    pub source: PromiseSource,
    pub holder_workflow_id: String,
    pub promise_id: String,
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
        call_id: ToolCallId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        completed_results: Vec<ToolInvocationResult>,
        spec: AwaitSpec,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_visible_context_entries: Vec<ContextEntryInput>,
    pub error_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffect>,
}

impl ToolInvocationResult {
    pub fn tool_result_context_entry(
        call_id: &ToolCallId,
        status: ToolCallStatus,
        content_ref: BlobRef,
    ) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::ToolResult {
                call_id: call_id.clone(),
                is_error: status.is_error(),
            },
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }
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
