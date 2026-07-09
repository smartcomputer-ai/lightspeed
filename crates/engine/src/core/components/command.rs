use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ContextEntryInput, ContextEntryKey, PromiseId, PromiseResolution, ResumeAwaitCommand, RunId,
    RunRequestCommand, SessionConfig, SubmitMessageCommand, ToolExecutionTarget, ToolName,
    ToolPatch, ToolSpec,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentCommand {
    OpenSession {
        config: SessionConfig,
    },
    /// Replace the session config with a complete document. The previous
    /// config is not consulted beyond validation (api-kind pinning) and the
    /// revision guard; anything omitted from the document reverts to
    /// defaults. Putting an identical document is an idempotent no-op.
    ReplaceSessionConfig {
        expected_revision: Option<u64>,
        config: SessionConfig,
    },
    ReplaceTools {
        expected_revision: Option<u64>,
        tools: BTreeMap<ToolName, ToolSpec>,
    },
    PatchTools {
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
        key: ContextEntryKey,
        entry: ContextEntryInput,
    },
    ReplaceContextPrefix {
        key_prefix: ContextEntryKey,
        entries: BTreeMap<ContextEntryKey, ContextEntryInput>,
    },
    RemoveContext {
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
