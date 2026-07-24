//! Core replay reducer for committed session events.

use crate::{CoreAgentEntry, CoreAgentEvent, CoreAgentState, DomainError, EventSeq};

pub fn apply_event(state: &mut CoreAgentState, entry: &CoreAgentEntry) -> Result<(), DomainError> {
    validate_next_position(state, entry)?;
    apply_event_kind(state, entry)?;
    state.reduced_to = Some(entry.position.clone());
    Ok(())
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

fn apply_event_kind(state: &mut CoreAgentState, entry: &CoreAgentEntry) -> Result<(), DomainError> {
    match &entry.event {
        CoreAgentEvent::Lifecycle(event) => {
            crate::core::components::lifecycle::apply_event(state, event)
        }
        CoreAgentEvent::Run(event) => crate::core::components::run::apply_event(state, event),
        CoreAgentEvent::Turn(event) => crate::core::components::turn::apply_event(state, event),
        CoreAgentEvent::Context(event) => {
            crate::core::components::context::apply_event(state, event)
        }
        CoreAgentEvent::ToolConfig(event) => {
            crate::core::components::tooling::apply_config_event(state, event)
        }
        CoreAgentEvent::Tool(event) => crate::core::components::tooling::apply_event(state, event),
        CoreAgentEvent::Promise(event) => {
            crate::core::components::promise::apply_event(state, event)
        }
        CoreAgentEvent::WorkflowPortConfig(event) => {
            crate::core::components::workflow_port::apply_config_event(state, event)
        }
        CoreAgentEvent::WorkflowPort(event) => {
            crate::core::components::workflow_port::apply_event(state, event)
        }
    }
}
