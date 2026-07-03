//! Core planning composition for deterministic session progress.

use crate::{
    CoreAgentEventProposal, CoreAgentState, PlanningError,
    core::components::{context, run, tooling, turn},
};

/// Consults the run, tool, context, and turn planners in order and returns
/// the first non-empty batch of proposals.
pub fn plan_next(state: &CoreAgentState) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    for plan in [
        run::plan_next,
        tooling::plan_next,
        context::plan_next,
        turn::plan_next,
    ] {
        let proposals = plan(state)?;
        if !proposals.is_empty() {
            return Ok(proposals);
        }
    }
    Ok(Vec::new())
}
