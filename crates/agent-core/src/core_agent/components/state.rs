use serde::{Deserialize, Serialize};

use crate::{
    ContextState, IdCursors, SessionPosition, ToolingState,
    core_agent::components::{lifecycle::LifecycleState, run::RunQueueState},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreAgentState {
    pub reduced_to: Option<SessionPosition>,
    pub id_cursors: IdCursors,
    pub lifecycle: LifecycleState,
    pub runs: RunQueueState,
    pub context: ContextState,
    pub tooling: ToolingState,
}

impl CoreAgentState {
    pub fn new() -> Self {
        Self {
            reduced_to: None,
            id_cursors: IdCursors::default(),
            lifecycle: LifecycleState::default(),
            runs: RunQueueState::default(),
            context: ContextState::default(),
            tooling: ToolingState::default(),
        }
    }
}

impl Default for CoreAgentState {
    fn default() -> Self {
        Self::new()
    }
}
