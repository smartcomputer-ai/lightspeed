use serde::{Deserialize, Serialize};

use crate::{
    ContextState, EnvironmentState, IdCursors, PromiseComponentState, SessionPosition,
    ToolingState, WorkflowToolState,
    core::components::{lifecycle::LifecycleState, run::RunsState},
};

/// The shape and meaning of a stored [`CoreAgentState`] checkpoint. A checkpoint of any
/// other version is ignored and the session replays its log, which costs that one load
/// and nothing else. Bump it when a change would let an old checkpoint load with a value
/// replay would have set differently: a new field filled from events already in the log,
/// a changed reduction, or a removed field something still needs. A field only new event
/// kinds can fill does not need it, because replay would leave it empty too.
pub const SESSION_CHECKPOINT_FORMAT_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreAgentState {
    pub reduced_to: Option<SessionPosition>,
    pub id_cursors: IdCursors,
    pub lifecycle: LifecycleState,
    pub runs: RunsState,
    pub context: ContextState,
    #[serde(default)]
    pub environment: EnvironmentState,
    pub tooling: ToolingState,
    #[serde(default)]
    pub promises: PromiseComponentState,
    #[serde(default)]
    pub workflow_tools: WorkflowToolState,
    #[serde(default)]
    pub code_tools: crate::CodeToolState,
}

impl CoreAgentState {
    pub fn new() -> Self {
        Self {
            reduced_to: None,
            id_cursors: IdCursors::default(),
            lifecycle: LifecycleState::default(),
            runs: RunsState::default(),
            context: ContextState::default(),
            environment: EnvironmentState::default(),
            tooling: ToolingState::default(),
            promises: PromiseComponentState::default(),
            workflow_tools: WorkflowToolState::default(),
            code_tools: crate::CodeToolState::default(),
        }
    }
}

impl Default for CoreAgentState {
    fn default() -> Self {
        Self::new()
    }
}
