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

use harness::{
    CoreAgentCodec, CoreAgentEntry, CoreAgentEvent, CoreAgentState, RunEvent, SubmissionId,
    storage::StoredSessionEntry,
};
use serde::{Deserialize, Serialize};

/// Outcome of reducing a session's persisted log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReducedSession {
    pub core_state: CoreAgentState,
    pub run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    /// Entries applied by this process; excluded from serialized checkpoints
    /// because a checkpoint restart counts only its own tail.
    #[serde(skip)]
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
    reduce_session_entries_from(ReducedSession::default(), entries)
}

/// Apply persisted entries onto an existing reduction. Checkpoint recovery
/// and the live workflow both use this accumulator, so tail replay has exactly
/// the same reducer semantics as a replay from sequence one.
pub fn reduce_session_entries_from(
    mut reduced: ReducedSession,
    entries: &[StoredSessionEntry],
) -> Result<ReducedSession, RehydrateError> {
    for entry in entries {
        let decoded = CoreAgentCodec
            .decode_entry(entry)
            .map_err(|error| RehydrateError::Decode(error.to_string()))?;
        accumulate_session_entry(&mut reduced, &decoded)?;
    }
    reduced.replayed_event_count = reduced
        .replayed_event_count
        .saturating_add(entries.len() as u64);
    Ok(reduced)
}

pub fn accumulate_session_entry(
    reduced: &mut ReducedSession,
    entry: &CoreAgentEntry,
) -> Result<(), RehydrateError> {
    if let CoreAgentEvent::Run(RunEvent::Accepted(accepted)) = &entry.event {
        reduced
            .run_submissions
            .insert(accepted.run_id.as_u64(), accepted.submission_id.clone());
    }
    harness::apply_event(&mut reduced.core_state, entry)
        .map_err(|error| RehydrateError::Apply(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        CoreAgentEntry, CoreAgentJoins, CoreAgentLifecycleEvent, EventSeq, ModelSelection,
        ProviderApiKind, SessionPosition,
    };

    #[test]
    fn empty_log_reduces_to_default_state() {
        let reduced = reduce_session_entries(&[]).expect("reduce empty");
        assert_eq!(reduced.replayed_event_count, 0);
        assert!(reduced.run_submissions.is_empty());
        assert_eq!(reduced.core_state, CoreAgentState::new());
    }

    #[test]
    fn every_checkpoint_cut_reduces_to_the_same_state() {
        let config = crate::default_session_config(ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "test".to_owned(),
            model: "test".to_owned(),
        });
        let events = [
            CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
            CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed),
        ];
        let entries = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| {
                CoreAgentCodec
                    .encode_entry(&CoreAgentEntry {
                        position: SessionPosition {
                            seq: EventSeq::new(index as u64 + 1),
                        },
                        observed_at_ms: index as u64 + 1,
                        joins: CoreAgentJoins::default(),
                        event,
                    })
                    .expect("encode entry")
            })
            .collect::<Vec<_>>();
        let full = reduce_session_entries(&entries).expect("reduce full log");

        for cut in 0..=entries.len() {
            let prefix = reduce_session_entries(&entries[..cut]).expect("reduce prefix");
            let resumed =
                reduce_session_entries_from(prefix, &entries[cut..]).expect("reduce suffix");
            assert_eq!(resumed, full, "cut point {cut}");
        }
    }
}
