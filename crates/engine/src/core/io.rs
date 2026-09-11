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
    ContextEntryKind, EnvironmentId, LlmGenerationFacts, LlmGenerationStatus, LlmRequest,
    PromiseId, PromiseOwnership, PromiseScope, PromiseStatus, RunId, SessionId, ToolBatchId,
    ToolCallId, ToolCallStatus, ToolExecutionSpec, ToolName, TurnId, WorkflowToolBinding,
    WorkspaceLink,
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

    /// Execute one call of an admitted tool batch.
    ///
    /// The default adapts through [`Self::invoke_batch`] for runtimes that
    /// execute batches as one unit; hosted runtimes override it with a real
    /// per-call path.
    async fn invoke_call(
        &self,
        request: ToolInvocationCallRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let outcome = self.invoke_batch(request.into_batch_request()).await?;
        outcome.completed_result()?.single_result()
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
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PromiseSourceCheckResult {
    Pending,
    Resolved { payload_ref: Option<BlobRef> },
    Failed { error_ref: Option<BlobRef> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationBatchRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_links: Vec<WorkspaceLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<EnvironmentId>,
    pub environment_policy: Option<EnvironmentPolicyRuntime>,
    /// Admitted sub-agent grant for this tool batch. Runtime executors pin
    /// it into sub-agent invocations instead of reconstructing the owning
    /// session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents_policy: Option<crate::SubagentsFeature>,
    /// First promise id the executors of this dispatch may mint (see
    /// `ActiveToolBatch::promise_id_base`). A batch-unit dispatch counts up
    /// from here across all its calls; a per-call dispatch gets its own
    /// slot (`base + call index`) and may create at most one promise.
    pub promise_id_base: u64,
    pub calls: Vec<ToolInvocationRequest>,
}

/// Admitted session policy needed while resolving live environment resources.
///
/// This is transient runtime input recorded on the activity request, not a
/// durable session document. Environment records and provider observations
/// remain live resolver reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentPolicyRuntime {
    pub version: u32,
    pub allowed_provider_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_registration_key_ids: Option<Vec<String>>,
}

impl EnvironmentPolicyRuntime {
    pub const VERSION: u32 = 2;

    pub fn new(
        allowed_provider_ids: Option<Vec<String>>,
        allowed_registration_key_ids: Option<Vec<String>>,
    ) -> Self {
        Self {
            version: Self::VERSION,
            allowed_provider_ids,
            working_directory: None,
            allowed_registration_key_ids,
        }
    }

    /// Lower the session's environments grant into the runtime policy.
    pub fn from_feature(feature: &crate::EnvironmentsFeature) -> Self {
        Self {
            working_directory: feature.working_directory.clone(),
            ..Self::new(feature.providers.clone(), feature.registration_keys.clone())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationRequest {
    pub call_id: ToolCallId,
    pub tool_id: Option<ToolName>,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
    /// Original turn inputs for the shared built-in resolver. Provider wire
    /// names and arguments remain on the call; no execution catalog is rebuilt.
    pub builtin: Option<BuiltinToolCallRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tool: Option<WorkflowToolCallRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise_control: Option<PromiseControlCallRuntime>,
    /// Native MCP routing facts for an injected or search-exposure call,
    /// materialized by the deterministic owner for every dispatch of the
    /// batch. Present on the batch request and on the per-call request
    /// alike, so batch-unit and per-call execution route the call the same
    /// way; absent for every other tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_mcp: Option<RemoteMcpCallRuntime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinToolCallRuntime {
    pub spec: crate::BuiltinToolSpec,
    pub model: crate::ModelSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMcpCallTarget {
    pub server_id: String,
    pub record_revision: u64,
    pub server_label: String,
    pub server_url: String,
    pub allowed_tools: Option<Vec<String>>,
    pub approval: crate::RemoteMcpApprovalPolicy,
    pub auth_ref: Option<crate::SecretRef>,
    pub auth_required: bool,
    pub allow_private_network: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RemoteMcpCallRuntime {
    Injected {
        target: RemoteMcpCallTarget,
        remote_tool_name: String,
        approval_decision: Option<bool>,
    },
    Search {
        targets: Vec<RemoteMcpCallTarget>,
        approval_decision: Option<bool>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseControlKind {
    Cancel,
    Detach,
}

impl PromiseControlKind {
    fn for_tool_id(tool_id: &ToolName) -> Option<Self> {
        match tool_id.as_str() {
            "concurrency.cancel" => Some(Self::Cancel),
            "concurrency.detach" => Some(Self::Detach),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseControlArgumentCall {
    pub call_id: ToolCallId,
    pub kind: PromiseControlKind,
    pub arguments_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseControlArgumentRequest {
    pub version: u32,
    pub calls: Vec<PromiseControlArgumentCall>,
}

impl PromiseControlArgumentRequest {
    pub const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PromiseControlArgumentCallFacts {
    Parsed {
        call_id: ToolCallId,
        promise_ids: Vec<PromiseId>,
    },
    Invalid {
        call_id: ToolCallId,
    },
}

impl PromiseControlArgumentCallFacts {
    pub fn call_id(&self) -> &ToolCallId {
        match self {
            Self::Parsed { call_id, .. } | Self::Invalid { call_id } => call_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseControlArgumentFacts {
    pub version: u32,
    pub calls: Vec<PromiseControlArgumentCallFacts>,
}

impl PromiseControlArgumentFacts {
    pub const VERSION: u32 = 1;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseControlCallRuntime {
    pub version: u32,
    pub controls: Vec<PromiseControlRuntime>,
}

impl PromiseControlCallRuntime {
    pub const VERSION: u32 = 1;

    pub fn v1(controls: Vec<PromiseControlRuntime>) -> Self {
        Self {
            version: Self::VERSION,
            controls,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseControlRuntime {
    pub promise_id: PromiseId,
    pub state: PromiseControlStateRuntime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PromiseControlStateRuntime {
    Unknown,
    Known {
        ownership: PromiseOwnership,
        scope: PromiseScope,
        promise_status: PromiseStatus,
    },
}

impl ToolInvocationBatchRequest {
    pub fn promise_control_argument_request(&self) -> Option<PromiseControlArgumentRequest> {
        let calls = self
            .calls
            .iter()
            .filter_map(|call| {
                call.tool_id
                    .as_ref()
                    .and_then(PromiseControlKind::for_tool_id)
                    .map(|kind| PromiseControlArgumentCall {
                        call_id: call.call_id.clone(),
                        kind,
                        arguments_ref: call.arguments_ref.clone(),
                    })
            })
            .collect::<Vec<_>>();
        (!calls.is_empty()).then_some(PromiseControlArgumentRequest {
            version: PromiseControlArgumentRequest::VERSION,
            calls,
        })
    }
}

/// Bounded runtime facts needed to execute one call of an admitted tool batch
/// as its own activity.
///
/// The record carries the stable session/run/turn/batch/call identity plus the
/// batch-scoped runtime facts the call needs. Sibling summaries let the
/// execution boundary enforce cross-call batch rules (environment-selection
/// exclusivity) without a batch-level activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationCallRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_links: Vec<WorkspaceLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<EnvironmentId>,
    pub environment_policy: Option<EnvironmentPolicyRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents_policy: Option<crate::SubagentsFeature>,
    /// The one promise id this call may mint: the batch base plus the
    /// call's index, so sibling per-call dispatches never collide.
    pub promise_id_base: u64,
    pub call: ToolInvocationRequest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sibling_calls: Vec<ToolCallSummary>,
    /// Execution policy facts selected from the admitted tool binding.
    #[serde(default)]
    pub execution: ToolExecutionSpec,
}

/// Bounded summary of one sibling call in the same tool batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub call_id: ToolCallId,
    pub tool_id: Option<ToolName>,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
}

impl ToolInvocationCallRequest {
    /// Rebuild an equivalent single-call batch request for runtimes and
    /// helpers that operate on the batch shape.
    pub fn into_batch_request(self) -> ToolInvocationBatchRequest {
        ToolInvocationBatchRequest {
            session_id: self.session_id,
            run_id: self.run_id,
            turn_id: self.turn_id,
            batch_id: self.batch_id,
            workspace_links: self.workspace_links,
            vfs_working_directory: self.vfs_working_directory,
            active_environment_id: self.active_environment_id,
            environment_policy: self.environment_policy,
            subagents_policy: self.subagents_policy,
            promise_id_base: self.promise_id_base,
            calls: vec![self.call],
        }
    }
}

impl ToolInvocationBatchRequest {
    /// Split one call out of this batch request, carrying batch-scoped runtime
    /// facts and bounded sibling summaries.
    pub fn call_request(
        &self,
        index: usize,
        execution: ToolExecutionSpec,
    ) -> Option<ToolInvocationCallRequest> {
        let call = self.calls.get(index)?.clone();
        let sibling_calls = self
            .calls
            .iter()
            .enumerate()
            .filter(|(sibling_index, _)| *sibling_index != index)
            .map(|(_, sibling)| ToolCallSummary {
                call_id: sibling.call_id.clone(),
                tool_id: sibling.tool_id.clone(),
                tool_name: sibling.tool_name.clone(),
                arguments_ref: sibling.arguments_ref.clone(),
            })
            .collect();
        Some(ToolInvocationCallRequest {
            session_id: self.session_id.clone(),
            run_id: self.run_id,
            turn_id: self.turn_id,
            batch_id: self.batch_id,
            workspace_links: self.workspace_links.clone(),
            vfs_working_directory: self.vfs_working_directory.clone(),
            active_environment_id: self.active_environment_id.clone(),
            environment_policy: self.environment_policy.clone(),
            subagents_policy: self.subagents_policy.clone(),
            promise_id_base: self.promise_id_base + index as u64,
            call,
            sibling_calls,
            execution,
        })
    }
}

/// Bounded session-owned facts needed to execute one admitted workflow-tool call.
///
/// This is transient runtime input, not durable session vocabulary. Carrying it
/// on the activity request makes retries use the exact binding and emission
/// count observed when the deterministic owner scheduled the batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolCallRuntime {
    pub version: u32,
    pub binding: WorkflowToolBinding,
    pub prior_emission_count: u32,
}

impl WorkflowToolCallRuntime {
    pub const VERSION: u32 = 1;

    pub fn v1(binding: WorkflowToolBinding, prior_emission_count: u32) -> Self {
        Self {
            version: Self::VERSION,
            binding,
            prior_emission_count,
        }
    }
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

/// One native MCP call of a batch that must receive a run-owned approval
/// decision before the worker performs any MCP wire I/O.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeMcpApprovalRequest {
    pub call_id: ToolCallId,
    pub subject: crate::ApprovalSubject,
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
    /// One or more native MCP calls are gated on approval. Every ungated
    /// sibling has already executed and reports its terminal result here;
    /// the gated calls (and an `await` sibling, which never defers while a
    /// decision is outstanding) stay pending and are re-dispatched once the
    /// run is unparked.
    AwaitingApproval {
        batch_id: ToolBatchId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        completed_results: Vec<ToolInvocationResult>,
        approvals: Vec<NativeMcpApprovalRequest>,
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
            Self::AwaitingApproval { batch_id, .. } => Err(CoreAgentIoError::Failed {
                message: format!(
                    "tool batch {batch_id} is awaiting approval instead of completing"
                ),
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
    /// Wall-clock milliseconds the executing runtime spent on this call.
    /// Stamped by the execution activity; carried into the durable
    /// `ToolCallResult` unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Bytes of model-visible text the tool produced before the runtime's
    /// projection budget was applied. Absent for synthetic results and
    /// executors that do not measure. Recorded telemetry only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// True when the projection cut the model-visible text to its budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
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
            content: crate::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CoreAgentIoError {
    #[error("core agent I/O failed: {message}")]
    Failed { message: String },
    /// The exact same operation may be retried by the runtime substrate.
    /// Carries only the behavioral decision — provider taxonomy stays in the
    /// client layer. Terminal `Failed` remains the safe default for errors
    /// without explicit transient evidence.
    #[error("core agent I/O failed (retryable): {message}")]
    Retryable {
        message: String,
        retry_after: Option<std::time::Duration>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_requests_carry_the_call_native_mcp_routing() {
        let mut batch = batch_request_with_calls(&["call_a", "call_b"]);
        let routing = RemoteMcpCallRuntime::Injected {
            target: RemoteMcpCallTarget {
                server_id: "echo".to_owned(),
                record_revision: 1,
                server_label: "echo".to_owned(),
                server_url: "https://echo.example.com/mcp".to_owned(),
                allowed_tools: None,
                approval: crate::RemoteMcpApprovalPolicy::Never,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            },
            remote_tool_name: "hello".to_owned(),
            approval_decision: Some(true),
        };
        batch.calls[1].remote_mcp = Some(routing.clone());

        let request = batch
            .call_request(1, ToolExecutionSpec::default())
            .expect("call request");

        assert_eq!(request.call.remote_mcp, Some(routing.clone()));
        assert_eq!(
            request.into_batch_request().calls[0].remote_mcp,
            Some(routing)
        );
    }

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

    fn batch_request_with_calls(call_ids: &[&str]) -> ToolInvocationBatchRequest {
        ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(2),
            batch_id: ToolBatchId::new(3),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: Some(EnvironmentId::new("environment-a")),
            environment_policy: Some(EnvironmentPolicyRuntime::new(None, None)),
            subagents_policy: None,
            calls: call_ids
                .iter()
                .map(|call_id| ToolInvocationRequest {
                    builtin: None,
                    call_id: ToolCallId::new(*call_id),
                    tool_id: Some(ToolName::new("tool")),
                    tool_name: ToolName::new("tool"),
                    arguments_ref: BlobRef::from_bytes(call_id.as_bytes()),
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
                })
                .collect(),
        }
    }

    #[test]
    fn call_request_splits_one_call_with_sibling_summaries_and_batch_facts() {
        let batch = batch_request_with_calls(&["call_a", "call_b", "call_c"]);

        let request = batch
            .call_request(1, ToolExecutionSpec::default())
            .expect("call request");

        assert_eq!(request.call.call_id, ToolCallId::new("call_b"));
        assert_eq!(request.batch_id, batch.batch_id);
        assert_eq!(request.active_environment_id, batch.active_environment_id);
        assert_eq!(
            request
                .sibling_calls
                .iter()
                .map(|sibling| sibling.call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["call_a", "call_c"]
        );
        assert!(
            batch
                .call_request(3, ToolExecutionSpec::default())
                .is_none()
        );

        let rebuilt = request.into_batch_request();
        assert_eq!(rebuilt.batch_id, batch.batch_id);
        assert_eq!(rebuilt.calls.len(), 1);
        assert_eq!(rebuilt.calls[0].call_id, ToolCallId::new("call_b"));
    }

    struct BatchOnlyTools;

    #[async_trait]
    impl CoreAgentTools for BatchOnlyTools {
        async fn invoke_batch(
            &self,
            request: ToolInvocationBatchRequest,
        ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
            let results = request
                .calls
                .iter()
                .map(|call| ToolInvocationResult {
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(call.arguments_ref.clone()),
                    model_visible_context_entries: Vec::new(),
                    error_ref: None,
                    effects: Vec::new(),
                })
                .collect();
            Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results,
            }))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_invoke_call_adapts_through_batch_execution() {
        let batch = batch_request_with_calls(&["call_a", "call_b"]);
        let request = batch
            .call_request(0, ToolExecutionSpec::default())
            .expect("call request");

        let result = BatchOnlyTools
            .invoke_call(request)
            .await
            .expect("invoke call");

        assert_eq!(result.call_id, ToolCallId::new("call_a"));
        assert_eq!(result.status, ToolCallStatus::Succeeded);
    }
}

#[cfg(test)]
mod promise_base_tests {
    use super::*;

    fn call(id: &str) -> ToolInvocationRequest {
        ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new(id),
            tool_id: Some(ToolName::new("sleep")),
            tool_name: ToolName::new("sleep"),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        }
    }

    #[test]
    fn per_call_requests_get_their_own_promise_slot_and_rebuild_the_batch_base() {
        let batch = ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-1"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: None,
            promise_id_base: 7,
            calls: vec![call("a"), call("b"), call("c")],
        };
        let third = batch
            .call_request(2, ToolExecutionSpec::default())
            .expect("third call");
        assert_eq!(third.promise_id_base, 9);
        assert_eq!(third.into_batch_request().promise_id_base, 9);
        assert!(
            batch
                .call_request(3, ToolExecutionSpec::default())
                .is_none()
        );
    }
}
