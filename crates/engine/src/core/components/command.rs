use serde::{Deserialize, Serialize};

use crate::{
    BlobRef, RunConfig, SessionConfig, SubmissionId, ToolExecutionTarget, ToolProfileId,
    ToolRegistry,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAgentCommand {
    OpenSession {
        config: SessionConfig,
    },
    UpdateSessionConfig {
        config: SessionConfig,
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
    RequestRun {
        submission_id: Option<SubmissionId>,
        input_ref: BlobRef,
        run_config: RunConfig,
    },
    RequestRunSteering {
        input_ref: BlobRef,
    },
    RequestRunCancellation,
    CloseSession,
}
