use serde::{Deserialize, Serialize};

use crate::{
    ApprovalId, BlobRef, ContextEntryInput, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins,
    CoreAgentState, CoreAgentStatus, DomainError, RunId, RunStatus, ToolCallId,
};

pub const MAX_APPROVAL_NOTE_BYTES: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalEvent {
    Requested {
        approval: ApprovalRequested,
    },
    RunParked {
        run_id: RunId,
    },
    Decided {
        approval_id: ApprovalId,
        run_id: RunId,
        decision: ApprovalDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<crate::Attribution>,
    },
    Cancelled {
        approval_id: ApprovalId,
        run_id: RunId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequested {
    pub approval_id: ApprovalId,
    pub run_id: RunId,
    pub subject: ApprovalSubject,
    pub continuation: ApprovalContinuation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApprovalSubject {
    McpToolCall {
        server_id: String,
        server_label: String,
        tool_name: String,
        arguments_ref: BlobRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApprovalContinuation {
    OpenAiMcp { provider_request_id: String },
    NativeMcp { call_id: ToolCallId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecisionCommand {
    pub approval_id: ApprovalId,
    pub run_id: RunId,
    pub decision: ApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Who decided, as the API boundary attributed the request.
    pub decided_by: Option<crate::Attribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ContextEntryInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedApprovalRequest {
    pub subject: ApprovalSubject,
    pub continuation: ApprovalContinuation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Cancelled,
}

impl ApprovalStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub request: ApprovalRequested,
    /// Wall-clock time of the request event, retained so summary readers do
    /// not have to rescan the event log for it.
    #[serde(default)]
    pub requested_at_ms: u64,
    pub status: ApprovalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Who decided, as the API boundary attributed the request.
    pub decided_by: Option<crate::Attribution>,
}

pub fn plan_approval_next(
    state: &CoreAgentState,
) -> Result<Vec<CoreAgentEventProposal>, crate::PlanningError> {
    if state.lifecycle.status != CoreAgentStatus::Open {
        return Ok(Vec::new());
    }
    let Some(run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    if run.status != RunStatus::Active
        || run.active_turn_id.is_some()
        || run.active_tool_batch_id.is_some()
        || run.pending_approvals().next().is_none()
        || has_unstarted_tool_calls(run)
    {
        return Ok(Vec::new());
    }
    Ok(vec![CoreAgentEventProposal::new(
        CoreAgentJoins {
            run_id: Some(run.run_id),
            ..CoreAgentJoins::default()
        },
        CoreAgentEvent::Approval(ApprovalEvent::RunParked { run_id: run.run_id }),
    )])
}

fn has_unstarted_tool_calls(run: &crate::ActiveRun) -> bool {
    run.turns.iter().any(|(turn_id, turn)| {
        turn.outcome == Some(crate::TurnOutcome::ToolCallsQueued)
            && turn
                .facts
                .as_ref()
                .is_some_and(|facts| !facts.tool_calls.is_empty())
            && !run
                .tool_batches
                .values()
                .any(|batch| batch.turn_id == *turn_id)
            && !run
                .completed_tool_batches
                .values()
                .any(|batch| batch.turn_id == *turn_id)
    })
}

pub(crate) fn apply_approval_event(
    state: &mut CoreAgentState,
    event: &ApprovalEvent,
    observed_at_ms: u64,
) -> Result<(), DomainError> {
    match event {
        ApprovalEvent::Requested { approval } => {
            let expected = state
                .id_cursors
                .last_approval_id
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::InvariantViolation("approval id cursor exhausted".into())
                })?;
            if approval.approval_id.as_str() != format!("approval_{expected}") {
                return Err(DomainError::InvariantViolation(format!(
                    "expected approval id approval_{expected}, got {}",
                    approval.approval_id
                )));
            }
            let run = state.runs.active.as_mut().ok_or_else(|| {
                DomainError::InvariantViolation("approval request requires an active run".into())
            })?;
            if run.run_id != approval.run_id || run.status != RunStatus::Active {
                return Err(DomainError::InvariantViolation(
                    "approval request does not match the active run".into(),
                ));
            }
            if run
                .approvals
                .values()
                .any(|record| record.request.continuation == approval.continuation)
            {
                return Err(DomainError::InvariantViolation(
                    "duplicate approval continuation".into(),
                ));
            }
            run.approvals.insert(
                approval.approval_id.clone(),
                ApprovalRecord {
                    request: approval.clone(),
                    requested_at_ms: observed_at_ms,
                    status: ApprovalStatus::Pending,
                    note: None,
                    decided_by: None,
                },
            );
            state.id_cursors.last_approval_id = expected;
            Ok(())
        }
        ApprovalEvent::RunParked { run_id } => {
            let run = crate::core::components::run::active_run_mut(state, *run_id)?;
            if run.pending_approvals().next().is_none() {
                return Err(DomainError::InvariantViolation(
                    "run cannot park without pending approvals".into(),
                ));
            }
            let native_batch_approval = run.active_tool_batch_id.is_some()
                && run.pending_approvals().all(|record| {
                    matches!(
                        record.request.continuation,
                        ApprovalContinuation::NativeMcp { .. }
                    )
                });
            if run.status != RunStatus::Active
                || run.active_turn_id.is_some()
                || (run.active_tool_batch_id.is_some() && !native_batch_approval)
            {
                return Err(DomainError::InvariantViolation(
                    "approval parking requires an idle active run".into(),
                ));
            }
            run.status = RunStatus::Parked;
            Ok(())
        }
        ApprovalEvent::Decided {
            approval_id,
            run_id,
            decision,
            note,
            decided_by,
        } => {
            validate_note(note.as_deref())?;
            let run = crate::core::components::run::active_run_mut(state, *run_id)?;
            let record = run.approvals.get_mut(approval_id).ok_or_else(|| {
                DomainError::InvariantViolation(format!("unknown approval {approval_id}"))
            })?;
            if record.request.run_id != *run_id || record.status != ApprovalStatus::Pending {
                return Err(DomainError::InvariantViolation(format!(
                    "approval {approval_id} is not pending for run {run_id}"
                )));
            }
            record.status = match decision {
                ApprovalDecision::Approved => ApprovalStatus::Approved,
                ApprovalDecision::Rejected => ApprovalStatus::Rejected,
            };
            record.note = note.clone();
            record.decided_by = decided_by.clone();
            if run.pending_approvals().next().is_none() && run.status == RunStatus::Parked {
                run.status = RunStatus::Active;
            }
            Ok(())
        }
        ApprovalEvent::Cancelled {
            approval_id,
            run_id,
        } => {
            let run = crate::core::components::run::active_run_mut(state, *run_id)?;
            let record = run.approvals.get_mut(approval_id).ok_or_else(|| {
                DomainError::InvariantViolation(format!("unknown approval {approval_id}"))
            })?;
            if record.request.run_id != *run_id || record.status != ApprovalStatus::Pending {
                return Err(DomainError::InvariantViolation(format!(
                    "approval {approval_id} is not pending for run {run_id}"
                )));
            }
            record.status = ApprovalStatus::Cancelled;
            Ok(())
        }
    }
}

pub fn validate_note(note: Option<&str>) -> Result<(), DomainError> {
    if let Some(note) = note {
        if note.trim() != note || note.is_empty() {
            return Err(DomainError::InvariantViolation(
                "approval note must be non-empty and trimmed when present".into(),
            ));
        }
        if note.len() > MAX_APPROVAL_NOTE_BYTES {
            return Err(DomainError::InvariantViolation(format!(
                "approval note is too long: {} bytes, max {MAX_APPROVAL_NOTE_BYTES}",
                note.len()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn active_state() -> CoreAgentState {
        let mut state = CoreAgentState::new();
        state.lifecycle.status = CoreAgentStatus::Open;
        state.runs.active = Some(crate::ActiveRun {
            run_id: RunId::new(1),
            status: RunStatus::Active,
            submission_id: None,
            source: crate::RunSource::Input { input: Vec::new() },
            input_entry_ids: Vec::new(),
            input_consumed_by_turn_id: None,
            run_config: crate::RunConfig::default(),
            config_revision: 0,
            first_seq: crate::EventSeq::new(1),
            accepted_at_ms: 1,
            started_at_ms: Some(1),
            usage: None,
            steering: Vec::new(),
            turns: BTreeMap::new(),
            active_turn_id: None,
            active_tool_batch_id: None,
            approvals: BTreeMap::new(),
            parked_tool_batch: None,
            tool_batches: BTreeMap::new(),
            completed_tool_batches: BTreeMap::new(),
            output: None,
            failure: None,
            notify_on_terminal: Vec::new(),
        });
        state
    }

    fn request(number: u64, provider_request_id: &str) -> ApprovalRequested {
        ApprovalRequested {
            approval_id: ApprovalId::try_new(format!("approval_{number}")).expect("approval id"),
            run_id: RunId::new(1),
            subject: ApprovalSubject::McpToolCall {
                server_id: "mail".to_owned(),
                server_label: "mail".to_owned(),
                tool_name: "send".to_owned(),
                arguments_ref: BlobRef::from_bytes(format!("{{\"n\":{number}}}").as_bytes()),
            },
            continuation: ApprovalContinuation::OpenAiMcp {
                provider_request_id: provider_request_id.to_owned(),
            },
        }
    }

    #[test]
    fn approvals_are_run_owned_single_use_and_unpark_only_as_a_set() {
        let mut state = active_state();
        for approval in [request(1, "mcpr_1"), request(2, "mcpr_2")] {
            apply_approval_event(&mut state, &ApprovalEvent::Requested { approval }, 7)
                .expect("request");
        }
        apply_approval_event(
            &mut state,
            &ApprovalEvent::RunParked {
                run_id: RunId::new(1),
            },
            8,
        )
        .expect("park");

        let decide = |number, decision| ApprovalEvent::Decided {
            approval_id: ApprovalId::try_new(format!("approval_{number}")).expect("approval id"),
            run_id: RunId::new(1),
            decision,
            note: None,
            decided_by: None,
        };
        apply_approval_event(&mut state, &decide(1, ApprovalDecision::Approved), 9)
            .expect("first decision");
        let run = state.runs.active.as_ref().expect("active run");
        assert_eq!(run.status, RunStatus::Parked);
        assert_eq!(run.pending_approvals().count(), 1);

        let duplicate =
            apply_approval_event(&mut state, &decide(1, ApprovalDecision::Rejected), 10)
                .expect_err("approval is single use");
        assert!(matches!(duplicate, DomainError::InvariantViolation(_)));

        apply_approval_event(&mut state, &decide(2, ApprovalDecision::Rejected), 11)
            .expect("last decision");
        let run = state.runs.active.as_ref().expect("active run");
        assert_eq!(run.status, RunStatus::Active);
        assert_eq!(run.approvals.len(), 2);
        assert_eq!(state.id_cursors.last_approval_id, 2);
    }

    #[test]
    fn cancellation_is_terminal_without_becoming_a_human_decision() {
        let mut state = active_state();
        let approval = request(1, "mcpr_1");
        let approval_id = approval.approval_id.clone();
        apply_approval_event(&mut state, &ApprovalEvent::Requested { approval }, 7)
            .expect("request");
        apply_approval_event(
            &mut state,
            &ApprovalEvent::Cancelled {
                approval_id: approval_id.clone(),
                run_id: RunId::new(1),
            },
            8,
        )
        .expect("cancel");

        let record = state
            .runs
            .active
            .as_ref()
            .expect("run")
            .approvals
            .get(&approval_id)
            .expect("approval");
        assert_eq!(record.status, ApprovalStatus::Cancelled);
        assert!(record.note.is_none());
        assert!(record.decided_by.is_none());
    }
}
