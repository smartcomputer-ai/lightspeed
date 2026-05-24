//! Core planning composition for deterministic session progress.

use crate::{CoreAgentEventProposal, CoreAgentState, PlanNext, PlanningError};

pub use crate::core::components::{
    context::CoreContextPlanner, run::CoreRunPlanner, tooling::CoreToolPlanner,
    turn::CoreTurnPlanner,
};

pub struct CorePlanner {
    layers: Vec<Box<dyn PlanNext>>,
}

impl CorePlanner {
    pub fn new(layers: Vec<Box<dyn PlanNext>>) -> Self {
        Self { layers }
    }

    pub fn core() -> Self {
        Self::new(vec![
            Box::new(CoreRunPlanner),
            Box::new(CoreToolPlanner),
            Box::new(CoreContextPlanner),
            Box::new(CoreTurnPlanner),
        ])
    }
}

impl Default for CorePlanner {
    fn default() -> Self {
        Self::core()
    }
}

impl PlanNext for CorePlanner {
    fn plan_next(
        &self,
        state: &CoreAgentState,
    ) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
        for layer in &self.layers {
            let proposals = layer.plan_next(state)?;
            if !proposals.is_empty() {
                return Ok(proposals);
            }
        }
        Ok(Vec::new())
    }
}
