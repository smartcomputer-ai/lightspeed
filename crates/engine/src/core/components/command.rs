use serde::{Deserialize, Serialize};

use crate::{
    ContextEntryInput, ContextEntryKey, RunConfig, SessionConfig, SessionConfigPatch,
    SkillActivation, SkillCatalogContext, SubmissionId, ToolExecutionTarget, ToolProfileId,
    ToolRegistry,
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
    SetToolRegistry {
        registry: ToolRegistry,
    },
    SelectToolProfile {
        profile_id: ToolProfileId,
    },
    SetDefaultToolTarget {
        target: ToolExecutionTarget,
    },
    ClearDefaultToolTarget {
        namespace: String,
    },
    SetSkillCatalog {
        catalog: Option<SkillCatalogContext>,
    },
    SetSkillActivations {
        activations: Vec<SkillActivation>,
    },
    UpsertContext {
        key: ContextEntryKey,
        entry: ContextEntryInput,
    },
    RemoveContext {
        key: ContextEntryKey,
    },
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
