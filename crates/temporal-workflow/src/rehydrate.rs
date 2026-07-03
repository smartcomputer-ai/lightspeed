//! Deterministic session-log reduction shared between the bootstrap activity
//! and the workflow.
//!
//! rehydration reduces the durable session log into the compact
//! `CoreAgentState` plus the workflow-only `run_submissions` index. Previously
//! the workflow pulled every persisted entry through the activity result and
//! reduced in-workflow; that transported the full log through Temporal history
//! and failed long-lived sessions. The reduction now happens inside the
//! activity using this helper, and only the compact result crosses the boundary.

use std::collections::BTreeMap;

use engine::{
    CoreAgentCodec, CoreAgentEntry, CoreAgentEvent, CoreAgentState, RunEvent, SubmissionId,
    storage::StoredSessionEntry,
};

/// Outcome of reducing a session's persisted log.
#[derive(Clone, Debug, Default)]
pub struct ReducedSession {
    pub core_state: CoreAgentState,
    pub run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    pub replayed_event_count: u64,
}

/// Error reducing the durable session log.
#[derive(Clone, Debug)]
pub enum RehydrateError {
    /// A persisted entry failed to decode with the CoreAgent codec.
    Decode(String),
    /// Applying a decoded entry violated a reducer invariant.
    Apply(String),
}

impl std::fmt::Display for RehydrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RehydrateError::Decode(message) => write!(f, "decode session entry: {message}"),
            RehydrateError::Apply(message) => write!(f, "apply session entry: {message}"),
        }
    }
}

impl std::error::Error for RehydrateError {}

/// Decode and reduce persisted session entries into compact agent state plus the
/// `run_id -> submission_id` index the workflow reconstructs from accepted-run
/// events. This is the single source of truth for replay; both the bootstrap
/// activity and any in-workflow cold path must use it so reduced state is
/// identical regardless of where replay runs.
pub fn reduce_session_entries(
    entries: &[StoredSessionEntry],
) -> Result<ReducedSession, RehydrateError> {
    let mut reduced = ReducedSession::default();
    for entry in entries {
        let decoded = CoreAgentCodec
            .decode_entry(entry)
            .map_err(|error| RehydrateError::Decode(error.to_string()))?;
        accumulate(&mut reduced, &decoded)?;
    }
    reduced.replayed_event_count = entries.len() as u64;
    Ok(reduced)
}

fn accumulate(reduced: &mut ReducedSession, entry: &CoreAgentEntry) -> Result<(), RehydrateError> {
    if let CoreAgentEvent::Run(RunEvent::Accepted(accepted)) = &entry.event {
        reduced
            .run_submissions
            .insert(accepted.run_id.as_u64(), accepted.submission_id.clone());
    }
    engine::apply_event(&mut reduced.core_state, entry)
        .map_err(|error| RehydrateError::Apply(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_log_reduces_to_default_state() {
        let reduced = reduce_session_entries(&[]).expect("reduce empty");
        assert_eq!(reduced.replayed_event_count, 0);
        assert!(reduced.run_submissions.is_empty());
        assert_eq!(reduced.core_state, CoreAgentState::new());
    }
}
