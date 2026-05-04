//! Core replay reducer for committed session events.

use crate::{
    ApplyEvent, CoreAgentEntry, CoreAgentEventKind, CoreAgentState, DomainError, EventSeq,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreApplyEvent;

impl ApplyEvent for CoreApplyEvent {
    fn apply(&self, state: &mut CoreAgentState, entry: &CoreAgentEntry) -> Result<(), DomainError> {
        validate_next_position(state, entry)?;
        apply_event(state, entry)?;
        state.reduced_to = Some(entry.position.clone());
        Ok(())
    }
}

fn validate_next_position(
    state: &CoreAgentState,
    entry: &CoreAgentEntry,
) -> Result<(), DomainError> {
    let expected_seq =
        match state.reduced_to.as_ref() {
            Some(position) => position.seq.as_u64().checked_add(1).ok_or_else(|| {
                DomainError::EventOrdering("session event sequence exhausted".into())
            })?,
            None => 1,
        };
    if entry.position.seq != EventSeq::new(expected_seq) {
        return Err(DomainError::EventOrdering(format!(
            "expected session event seq {}, got {}",
            expected_seq, entry.position.seq
        )));
    }
    Ok(())
}

fn apply_event(state: &mut CoreAgentState, entry: &CoreAgentEntry) -> Result<(), DomainError> {
    match &entry.event.kind {
        CoreAgentEventKind::Lifecycle(event) => {
            crate::core_agent::components::lifecycle::apply_event(state, event)
        }
        CoreAgentEventKind::Run(event) => {
            crate::core_agent::components::run::apply_event(state, event)
        }
        CoreAgentEventKind::Turn(event) => {
            crate::core_agent::components::turn::apply_event(state, event)
        }
        CoreAgentEventKind::Context(event) => {
            crate::core_agent::components::context::apply_event(state, event)
        }
        CoreAgentEventKind::ToolConfig(event) => {
            crate::core_agent::components::tooling::apply_config_event(state, event)
        }
        CoreAgentEventKind::Tool(event) => {
            crate::core_agent::components::tooling::apply_event(state, event)
        }
    }
}
