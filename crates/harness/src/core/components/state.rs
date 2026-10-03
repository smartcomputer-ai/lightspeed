use serde::{Deserialize, Serialize};

use crate::{
    ContextState, EnvironmentState, IdCursors, PromiseComponentState, SessionPosition,
    ToolingState, WorkflowToolState,
    core::components::{lifecycle::LifecycleState, run::RunQueueState},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreAgentState {
    pub reduced_to: Option<SessionPosition>,
    pub id_cursors: IdCursors,
    pub lifecycle: LifecycleState,
    pub runs: RunQueueState,
    pub context: ContextState,
    #[serde(default)]
    pub environment: EnvironmentState,
    pub tooling: ToolingState,
    #[serde(default)]
    pub promises: PromiseComponentState,
    #[serde(default)]
    pub workflow_tools: WorkflowToolState,
}

impl CoreAgentState {
    pub fn new() -> Self {
        Self {
            reduced_to: None,
            id_cursors: IdCursors::default(),
            lifecycle: LifecycleState::default(),
            runs: RunQueueState::default(),
            context: ContextState::default(),
            environment: EnvironmentState::default(),
            tooling: ToolingState::default(),
            promises: PromiseComponentState::default(),
            workflow_tools: WorkflowToolState::default(),
        }
    }
}

impl Default for CoreAgentState {
    fn default() -> Self {
        Self::new()
    }
}
