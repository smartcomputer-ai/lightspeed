use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ContextEntryInput, ContextEntryKey, RunConfig, SessionConfig, SessionConfigPatch, SubmissionId,
    ToolExecutionTarget, ToolName, ToolPatch, ToolSpec,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentCommand {
    OpenSession {
        config: SessionConfig,
    },
    PatchSessionConfig {
        expected_revision: Option<u64>,
        patch: SessionConfigPatch,
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
    RequestRun {
        submission_id: Option<SubmissionId>,
        input: Vec<ContextEntryInput>,
        run_config: RunConfig,
    },
    RequestRunSteering {
        input: Vec<ContextEntryInput>,
    },
    RequestRunCancellation,
    CloseSession,
}
