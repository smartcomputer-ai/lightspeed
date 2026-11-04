//! Core replay reducer for committed session events.

use crate::{CoreAgentEntry, CoreAgentEvent, CoreAgentState, DomainError, EventSeq};

pub fn apply_event(state: &mut CoreAgentState, entry: &CoreAgentEntry) -> Result<(), DomainError> {
    validate_next_position(state, entry)?;
    apply_event_kind(state, entry)?;
    state.reduced_to = Some(entry.position.clone());
    #[cfg(test)]
    assert_state_survives_checkpoint(state);
    Ok(())
}

/// A checkpoint and every workflow bootstrap carry the state as JSON, so a field that does
/// not survive this changes a session at its next rollover. Every unit test in this crate
/// that reduces an event checks it; nothing outside them pays for it.
#[cfg(test)]
fn assert_state_survives_checkpoint(state: &CoreAgentState) {
    let encoded = serde_json::to_vec(state).expect("reduced state encodes");
    let decoded: CoreAgentState = serde_json::from_slice(&encoded).expect("reduced state decodes");
    assert_eq!(&decoded, state, "reduced state changed crossing JSON");
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
        CoreAgentEvent::Run(event) => crate::core::components::run::apply_event(
            state,
            event,
            entry.position.seq,
            entry.observed_at_ms,
        ),
        CoreAgentEvent::Approval(event) => crate::core::components::approval::apply_approval_event(
            state,
            event,
            entry.observed_at_ms,
        ),
        CoreAgentEvent::Turn(event) => crate::core::components::turn::apply_event(state, event),
        CoreAgentEvent::Context(event) => {
            crate::core::components::context::apply_event(state, event)
        }
        CoreAgentEvent::Environment(event) => {
            crate::core::components::environment::apply_event(state, event)
        }
        CoreAgentEvent::ToolConfig(event) => {
            crate::core::components::tooling::apply_config_event(state, event)
        }
        CoreAgentEvent::Tool(event) => crate::core::components::tooling::apply_event(state, event),
        CoreAgentEvent::Promise(event) => {
            crate::core::components::promise::apply_event(state, event)
        }
        CoreAgentEvent::WorkflowToolConfig(event) => {
            crate::core::components::workflow_tool::apply_config_event(state, event)
        }
        CoreAgentEvent::CodeTool(event) => {
            crate::core::components::code_tool::apply_code_tool_event(state, event)
        }
        CoreAgentEvent::WorkflowTool(event) => {
            crate::core::components::workflow_tool::apply_event(state, event)
        }
    }
}
