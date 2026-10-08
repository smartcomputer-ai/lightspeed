//! Generic session-owned code tool invocation protocol.
//!
//! A trusted execution host narrows the session's existing grants when opening
//! a scope. Subsequent requests name only an opaque binding from that scope;
//! they cannot supply their own argument adapter or execution destination.

use std::collections::{BTreeMap, BTreeSet};

use harness::{Attachment, BlobRef, ToolCallId, ToolName, WorkflowToolInvocationId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use temporalio_client::{
    Client, RpcOptions, WorkflowExecuteUpdateOptions, WorkflowQueryOptions,
    errors::{WorkflowQueryError, WorkflowUpdateError},
};

use crate::AgentSessionWorkflow;

pub const OPEN_CODE_TOOL_SCOPE_UPDATE: &str = "open_code_tool_scope";
pub const INVOKE_CODE_TOOL_UPDATE: &str = "invoke_code_tool";
pub const CLOSE_CODE_TOOL_SCOPE_UPDATE: &str = "close_code_tool_scope";
pub const CODE_TOOL_SCOPE_REPORT_QUERY: &str = "code_tool_scope_report";

/// Trusted host request to open a scope beneath an admitted joined invocation.
/// The session resolves bindings itself; this allowlist can only narrow grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeToolScopeRequest {
    pub execution_id: String,
    pub parent_invocation_id: WorkflowToolInvocationId,
    /// A supplied set narrows logical tool identities; an empty set grants no
    /// tools. None selects all currently host-callable tools except the parent
    /// operation. Reopening an existing scope retains its original bindings.
    #[serde(default)]
    pub allowed_tools: Option<BTreeSet<ToolName>>,
    #[schemars(range(min = 1))]
    pub max_calls: u32,
    #[schemars(range(min = 1))]
    pub max_in_flight: u32,
}

/// Small request forwarded by a trusted host after serializing guest arguments.
/// Identity is execution-local. Reusing it with different arguments or a
/// different binding is rejected by the session, including across RPC retries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeCodeToolRequest {
    pub execution_id: String,
    pub request_id: String,
    pub binding_id: String,
    pub arguments_ref: BlobRef,
}

/// Close admission immediately. Cancellation additionally requests cleanup of
/// admitted work; closing a scope does not roll back any completed effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseCodeToolScopeRequest {
    pub execution_id: String,
    pub cancel_pending: bool,
}

/// Read an authoritative snapshot, including after a runner loses its waiters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeToolScopeReportRequest {
    pub execution_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeToolScopeReport {
    pub execution_id: String,
    pub closed: bool,
    pub cancel_requested: bool,
    /// Exposed names keyed by the opaque handles accepted by invoke requests.
    pub bindings: BTreeMap<String, ToolName>,
    /// Requests are keyed by execution-local request id, never arrival order.
    pub calls: BTreeMap<String, CodeToolCallOutcome>,
}

impl From<&harness::CodeToolScope> for CodeToolScopeReport {
    fn from(scope: &harness::CodeToolScope) -> Self {
        Self {
            execution_id: scope.spec.execution_id.clone(),
            closed: scope.closed,
            cancel_requested: scope.cancel_requested,
            bindings: scope
                .spec
                .bindings
                .iter()
                .map(|(id, binding)| (id.clone(), binding.tool_name.clone()))
                .collect(),
            calls: scope
                .calls
                .iter()
                .map(|(id, call)| (id.clone(), CodeToolCallOutcome::from(call)))
                .collect(),
        }
    }
}

/// Script-facing completion metadata. Session effects, model context, and
/// activity configuration remain private to the owning session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeToolCallOutcome {
    pub request_id: String,
    pub call_id: ToolCallId,
    pub status: CodeToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

impl From<&harness::CodeToolCall> for CodeToolCallOutcome {
    fn from(call: &harness::CodeToolCall) -> Self {
        let mut outcome = Self {
            request_id: call.spec.origin.request_id.clone(),
            call_id: call.call_id.clone(),
            status: CodeToolCallStatus::Pending,
            output_ref: None,
            error_ref: None,
            attachments: Vec::new(),
        };
        match &call.status {
            harness::CodeToolCallStatus::Pending => {}
            harness::CodeToolCallStatus::Waiting { .. } => {
                outcome.status = CodeToolCallStatus::Waiting;
            }
            harness::CodeToolCallStatus::Completed { result } => {
                outcome.status = match result.status {
                    harness::ToolCallStatus::Succeeded => CodeToolCallStatus::Succeeded,
                    harness::ToolCallStatus::Cancelled => CodeToolCallStatus::Cancelled,
                    harness::ToolCallStatus::Unavailable => CodeToolCallStatus::Unavailable,
                    _ => CodeToolCallStatus::Failed,
                };
                outcome.output_ref = result.output_ref.clone();
                outcome.error_ref = result.error_ref.clone();
                outcome.attachments = result.attachments.clone();
            }
        }
        outcome
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeToolCallStatus {
    Pending,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    Unavailable,
}

impl CodeToolCallStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Waiting)
    }
}

/// Admission failures are successful protocol replies, distinct from transport
/// failures and the tool's own failed result. They never imply tool execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodeToolRejection {
    pub kind: CodeToolRejectionKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeToolRejectionKind {
    InvalidRequest,
    UnknownScope,
    ScopeClosed,
    Conflict,
    Unavailable,
    PermissionDenied,
    LimitExceeded,
    SessionNotReady,
    Internal,
}

impl CodeToolRejection {
    pub fn new(kind: CodeToolRejectionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CodeToolRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CodeToolRejection {}

pub type CodeToolScopeResult = Result<CodeToolScopeReport, CodeToolRejection>;
pub type CodeToolInvocationResult = Result<CodeToolCallOutcome, CodeToolRejection>;

/// Temporal is the transport even when the runner and session share a process.
/// Dropping one of these futures only drops the client waiter; explicit scope
/// closure requests cancellation of already admitted operations.
#[derive(Clone)]
pub struct CodeToolClient {
    client: Client,
    session_workflow_id: String,
}

impl CodeToolClient {
    /// Use the stable workflow id, without a run id. Session-side request
    /// identity remains authoritative across workflow replay and rollover.
    pub fn new(client: Client, session_workflow_id: String) -> Self {
        Self {
            client,
            session_workflow_id,
        }
    }

    pub async fn open_scope(
        &self,
        request: OpenCodeToolScopeRequest,
        rpc_options: RpcOptions,
    ) -> Result<CodeToolScopeResult, WorkflowUpdateError> {
        let options = update_options(OPEN_CODE_TOOL_SCOPE_UPDATE, &request, rpc_options);
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(self.session_workflow_id.clone())
            .execute_update(AgentSessionWorkflow::open_code_tool_scope, request, options)
            .await
    }

    pub async fn invoke(
        &self,
        request: InvokeCodeToolRequest,
        rpc_options: RpcOptions,
    ) -> Result<CodeToolInvocationResult, WorkflowUpdateError> {
        let options = update_options(INVOKE_CODE_TOOL_UPDATE, &request, rpc_options);
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(self.session_workflow_id.clone())
            .execute_update(AgentSessionWorkflow::invoke_code_tool, request, options)
            .await
    }

    pub async fn close_scope(
        &self,
        request: CloseCodeToolScopeRequest,
        rpc_options: RpcOptions,
    ) -> Result<CodeToolScopeResult, WorkflowUpdateError> {
        let options = update_options(CLOSE_CODE_TOOL_SCOPE_UPDATE, &request, rpc_options);
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(self.session_workflow_id.clone())
            .execute_update(
                AgentSessionWorkflow::close_code_tool_scope,
                request,
                options,
            )
            .await
    }

    /// Queries do not reuse the cached result of an earlier Update. Repeated
    /// reports can therefore observe effects that finish after scope closure.
    pub async fn report(
        &self,
        request: CodeToolScopeReportRequest,
        rpc_options: RpcOptions,
    ) -> Result<CodeToolScopeResult, WorkflowQueryError> {
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(self.session_workflow_id.clone())
            .query(
                AgentSessionWorkflow::code_tool_scope_report,
                request,
                WorkflowQueryOptions::builder()
                    .rpc_options(rpc_options)
                    .build(),
            )
            .await
    }
}

fn update_options(
    operation: &str,
    request: &impl Serialize,
    rpc_options: RpcOptions,
) -> WorkflowExecuteUpdateOptions {
    WorkflowExecuteUpdateOptions::builder()
        .update_id(update_id(operation, request))
        .rpc_options(rpc_options)
        .build()
}

fn update_id(operation: &str, request: &impl Serialize) -> String {
    let request = serde_json::to_vec(request).expect("code tool request has a JSON wire encoding");
    let mut hasher = Sha256::new();
    for part in [
        b"lightspeed.code-tool.update.v1".as_slice(),
        operation.as_bytes(),
        &request,
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    // Include the full request, not just its identity. Otherwise Temporal could
    // return a cached success for a conflicting reuse before session admission
    // sees the changed arguments and rejects it.
    format!("ctu:sha256:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_identity_retries_exact_requests_but_preserves_conflict_checks() {
        let request = InvokeCodeToolRequest {
            execution_id: "execution-a".to_owned(),
            request_id: "request-a".to_owned(),
            binding_id: "binding-a".to_owned(),
            arguments_ref: BlobRef::from_bytes(b"{}"),
        };
        let id = update_id(INVOKE_CODE_TOOL_UPDATE, &request);
        assert_eq!(id, update_id(INVOKE_CODE_TOOL_UPDATE, &request));
        for different in [
            InvokeCodeToolRequest {
                execution_id: "execution-b".to_owned(),
                ..request.clone()
            },
            InvokeCodeToolRequest {
                request_id: "request-b".to_owned(),
                ..request.clone()
            },
            InvokeCodeToolRequest {
                binding_id: "binding-b".to_owned(),
                ..request.clone()
            },
            InvokeCodeToolRequest {
                arguments_ref: BlobRef::from_bytes(b"{\"changed\":true}"),
                ..request.clone()
            },
        ] {
            assert_ne!(id, update_id(INVOKE_CODE_TOOL_UPDATE, &different));
        }
        assert_ne!(id, update_id(CLOSE_CODE_TOOL_SCOPE_UPDATE, &request));
    }
}
