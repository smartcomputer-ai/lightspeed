use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ContextEntryInput, ContextEntryKey, ManagedSessionWorkflowPorts, PromiseId, PromiseResolution,
    ResumeAwaitCommand, RunId, RunRequestCommand, SessionConfig, SubmitMessageCommand,
    ToolExecutionTarget, ToolName, ToolPatch, ToolSpec,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentCommand {
    OpenSession {
        config: SessionConfig,
    },
    /// Trusted managed-session creation path. The lifecycle controller and
    /// independently addressed workflow ports are admitted once, atomically
    /// with the lifecycle open event.
    OpenManagedSession {
        config: SessionConfig,
        session_universe_id: Uuid,
        workflow_ports: ManagedSessionWorkflowPorts,
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
    SetDefaultToolTarget {
        target: ToolExecutionTarget,
    },
    ClearDefaultToolTarget {
        namespace: String,
    },
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
    SubmitMessage(SubmitMessageCommand),
    RequestRunSteering {
        input: Vec<ContextEntryInput>,
    },
    /// Cancel one run owned by this session. Queued runs are dequeued as
    /// cancelled; the active run enters the normal cancellation funnel; a
    /// terminal or unknown run is an idempotent no-op.
    CancelRun {
        run_id: RunId,
    },
    /// Force the matching active run to `cancelled` regardless of open turn
    /// or tool-batch state. Watchdog/recovery surface: admission is an
    /// idempotent no-op when the run is no longer active.
    ForceCancelRun {
        run_id: RunId,
    },
    ResumeAwait(ResumeAwaitCommand),
    /// Deliver a promise resolution. All transports converge here; a promise
    /// that is already terminal makes this an idempotent no-op
    /// (first-writer-wins).
    ResolvePromise {
        promise_id: PromiseId,
        resolution: PromiseResolution,
    },
    CloseSession {
        /// Force-cancel the active run and drop queued runs before closing
        /// instead of rejecting on active work.
        #[serde(default)]
        force: bool,
    },
}
