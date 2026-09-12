use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ApprovalDecisionCommand, BlobRef, ContextEntryInput, ContextEntryKey, EnvironmentId,
    ManagedSessionWorkflowTools, PromiseId, PromiseResolution, ResumeToolBatchCommand, RunId,
    RunRequestCommand, SessionConfig, ToolName, ToolPatch, ToolSpec, WorkflowToolDeclaration,
    WorkflowToolInvocationId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentCommand {
    OpenSession {
        config: SessionConfig,
    },
    /// Trusted managed-session creation path. The lifecycle controller and
    /// independently addressed workflow tools are admitted once, atomically
    /// with the lifecycle open event.
    OpenManagedSession {
        config: SessionConfig,
        session_universe_id: Uuid,
        workflow_tools: ManagedSessionWorkflowTools,
    },
    /// Trusted runtime admission for one system-owned workflow-backed tool.
    /// Unlike managed-session declarations, system bindings may be added to
    /// an already-open session and do not imply lifecycle ownership. They are
    /// immutable once admitted; an identical command is an idempotent no-op.
    AdmitSystemWorkflowTool {
        session_universe_id: Uuid,
        declaration: WorkflowToolDeclaration,
    },
    /// Replace the session config with a complete document. The previous
    /// config is not consulted beyond validation (api-kind pinning) and the
    /// revision guard; anything omitted from the document reverts to
    /// defaults. Putting an identical document is an idempotent no-op.
    ReplaceSessionConfig {
        #[serde(default)]
        expected_revision: Option<u64>,
        config: SessionConfig,
    },
    ReplaceTools {
        #[serde(default)]
        expected_revision: Option<u64>,
        tools: BTreeMap<ToolName, ToolSpec>,
    },
    PatchTools {
        #[serde(default)]
        expected_revision: Option<u64>,
        patch: ToolPatch,
    },
    SetActiveEnvironment {
        environment_id: EnvironmentId,
    },
    ClearActiveEnvironment,
    UpsertContext {
        #[serde(default)]
        expected_revision: Option<u64>,
        key: ContextEntryKey,
        entry: ContextEntryInput,
    },
    ReplaceContextPrefix {
        #[serde(default)]
        expected_revision: Option<u64>,
        key_prefix: ContextEntryKey,
        entries: BTreeMap<ContextEntryKey, ContextEntryInput>,
    },
    RemoveContext {
        #[serde(default)]
        expected_revision: Option<u64>,
        key: ContextEntryKey,
    },
    CompactContext,
    RequestRun(RunRequestCommand),
    RequestRunSteering {
        #[serde(default = "legacy_untargeted_steering_run_id")]
        run_id: RunId,
        input: Vec<ContextEntryInput>,
    },
    /// Cancel one run owned by this session. Queued runs are dequeued as
    /// cancelled; the active run enters the normal cancellation funnel; a
    /// terminal or unknown run is an idempotent no-op.
    CancelRun {
        run_id: RunId,
    },
    DecideApproval(ApprovalDecisionCommand),
    /// Force the matching active run to `cancelled` regardless of open turn
    /// or tool-batch state. Watchdog/recovery surface: admission is an
    /// idempotent no-op when the run is no longer active.
    ForceCancelRun {
        run_id: RunId,
    },
    ResumeToolBatch(ResumeToolBatchCommand),
    /// Deliver a promise resolution. All transports converge here; a promise
    /// that is already terminal makes this an idempotent no-op
    /// (first-writer-wins).
    ResolvePromise {
        promise_id: PromiseId,
        resolution: PromiseResolution,
    },
    /// Terminal push-delivery failure for one promise-bearing workflow-tool
    /// invocation: atomically records `WorkflowTool::DeliveryFailed` and
    /// fails every still-pending completion promise of that invocation, so a
    /// dead receiver can never leave an unresolvable promise. Re-admission
    /// with the same error is an idempotent no-op.
    FailWorkflowToolDelivery {
        invocation_id: WorkflowToolInvocationId,
        error_ref: BlobRef,
    },
    /// Terminal start failure for one start-on-call invocation: atomically
    /// records `WorkflowTool::StartFailed` and fails every still-pending
    /// keyed completion promise of that invocation. Re-admission with the
    /// same error is an idempotent no-op.
    FailWorkflowToolStart {
        invocation_id: WorkflowToolInvocationId,
        error_ref: BlobRef,
    },
    CloseSession {
        /// Force-cancel the active run and drop queued runs before closing
        /// instead of rejecting on active work.
        #[serde(default)]
        force: bool,
    },
}

// Older workflow signals did not carry a steering target. Zero never names an
// admitted run; only historical workflow replay may resolve this sentinel.
fn legacy_untargeted_steering_run_id() -> RunId {
    RunId::new(0)
}
