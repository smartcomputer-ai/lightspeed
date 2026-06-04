use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActiveToolBatch, BlobRef, CompletedToolBatch, ContextEntryInput, CoreAgentEventKind,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus, DomainError, PlanNext,
    PlanningError, RunConfig, RunId, SkillActivationScope, SubmissionId, ToolBatchId, TurnId,
    TurnOutcome, TurnState, TurnStatus,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Accepted {
        run_id: RunId,
        submission_id: Option<SubmissionId>,
        input: Vec<ContextEntryInput>,
        run_config: RunConfig,
        config_revision: u64,
    },
    Started {
        run_id: RunId,
    },
    SteeringAccepted {
        run_id: RunId,
        input: Vec<ContextEntryInput>,
    },
    CancellationRequested {
        run_id: RunId,
    },
    Completed {
        run_id: RunId,
        output_ref: Option<BlobRef>,
    },
    Failed {
        run_id: RunId,
        failure: RunFailure,
    },
    Cancelled {
        run_id: RunId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedRun {
    pub run_id: RunId,
    pub submission_id: Option<SubmissionId>,
    pub input: Vec<ContextEntryInput>,
    pub run_config: RunConfig,
    pub config_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRun {
    pub run_id: RunId,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    pub input: Vec<ContextEntryInput>,
    pub run_config: RunConfig,
    pub config_revision: u64,
    pub steering: Vec<Vec<ContextEntryInput>>,
    pub turns: BTreeMap<TurnId, TurnState>,
    pub active_turn_id: Option<TurnId>,
    pub active_tool_batch_id: Option<ToolBatchId>,
    pub tool_batches: BTreeMap<ToolBatchId, ActiveToolBatch>,
    pub completed_tool_batches: BTreeMap<ToolBatchId, CompletedToolBatch>,
    pub output_ref: Option<BlobRef>,
    pub failure: Option<RunFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    pub output_ref: Option<BlobRef>,
    pub failure: Option<RunFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    pub kind: RunFailureKind,
    pub message_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureKind {
    ModelFailure,
    ToolFailure,
    ContextFailure,
    LimitExceeded,
    Cancelled,
    Internal,
}

pub type RunEvent = Event;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunQueueState {
    pub active: Option<ActiveRun>,
    pub queued: Vec<AcceptedRun>,
    pub completed: Vec<RunRecord>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreRunPlanner;

impl PlanNext for CoreRunPlanner {
    fn plan_next(
        &self,
        state: &CoreAgentState,
    ) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
        if state.lifecycle.status != CoreAgentStatus::Open {
            return Ok(Vec::new());
        }

        if let Some(active_run) = state.runs.active.as_ref() {
            if active_run.active_turn_id.is_none() && active_run.active_tool_batch_id.is_none() {
                if active_run.status == RunStatus::Cancelling {
                    let joins = CoreAgentJoins {
                        run_id: Some(active_run.run_id),
                        ..CoreAgentJoins::default()
                    };
                    return Ok(vec![CoreAgentEventProposal::new(
                        joins,
                        CoreAgentEventKind::Run(Event::Cancelled {
                            run_id: active_run.run_id,
                        }),
                    )]);
                }
                if active_run.status == RunStatus::Active {
                    if let Some(proposal) = terminal_run_proposal(active_run)? {
                        return Ok(vec![proposal]);
                    }
                }
            }
            return Ok(Vec::new());
        }

        let Some(queued) = state.runs.queued.first() else {
            return Ok(Vec::new());
        };

        let joins = CoreAgentJoins {
            run_id: Some(queued.run_id),
            submission_id: queued.submission_id.clone(),
            ..CoreAgentJoins::default()
        };
        let kind = CoreAgentEventKind::Run(Event::Started {
            run_id: queued.run_id,
        });

        Ok(vec![CoreAgentEventProposal::new(joins, kind)])
    }
}

fn terminal_run_proposal(
    active_run: &ActiveRun,
) -> Result<Option<CoreAgentEventProposal>, PlanningError> {
    let Some((turn_id, turn)) = active_run.turns.iter().next_back() else {
        return Ok(None);
    };
    let kind = match (&turn.status, turn.outcome.as_ref()) {
        (TurnStatus::Completed, Some(TurnOutcome::FinalOutput { output_ref })) => {
            Some(CoreAgentEventKind::Run(Event::Completed {
                run_id: active_run.run_id,
                output_ref: output_ref.clone(),
            }))
        }
        (TurnStatus::Failed, Some(TurnOutcome::Failed { failure_ref })) => {
            Some(CoreAgentEventKind::Run(Event::Failed {
                run_id: active_run.run_id,
                failure: RunFailure {
                    kind: RunFailureKind::ModelFailure,
                    message_ref: failure_ref.clone(),
                },
            }))
        }
        (TurnStatus::Cancelled, Some(TurnOutcome::Cancelled)) => {
            Some(CoreAgentEventKind::Run(Event::Failed {
                run_id: active_run.run_id,
                failure: RunFailure {
                    kind: RunFailureKind::Cancelled,
                    message_ref: None,
                },
            }))
        }
        (
            TurnStatus::Completed,
            Some(TurnOutcome::ToolCallsQueued | TurnOutcome::ContextUpdateRequired),
        ) => None,
        (
            TurnStatus::Failed | TurnStatus::Cancelled,
            Some(TurnOutcome::ToolCallsQueued | TurnOutcome::ContextUpdateRequired),
        ) => {
            return Err(DomainError::InvariantViolation(format!(
                "turn {} status {:?} does not match outcome {:?}",
                turn_id, turn.status, turn.outcome
            ))
            .into());
        }
        (
            TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Cancelled,
            Some(
                TurnOutcome::FinalOutput { .. }
                | TurnOutcome::Failed { .. }
                | TurnOutcome::Cancelled,
            ),
        ) => {
            return Err(DomainError::InvariantViolation(format!(
                "turn {} status {:?} does not match outcome {:?}",
                turn_id, turn.status, turn.outcome
            ))
            .into());
        }
        (TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Cancelled, None) => {
            return Err(DomainError::InvariantViolation(format!(
                "terminal turn {} is missing outcome",
                turn_id
            ))
            .into());
        }
        _ => None,
    };

    Ok(kind.map(|kind| {
        CoreAgentEventProposal::new(
            CoreAgentJoins {
                run_id: Some(active_run.run_id),
                turn_id: Some(*turn_id),
                ..CoreAgentJoins::default()
            },
            kind,
        )
    }))
}

pub(crate) fn latest_turn_is_terminal_run_outcome(
    active_run: &ActiveRun,
) -> Result<bool, PlanningError> {
    Ok(terminal_run_proposal(active_run)?.is_some())
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::Accepted {
            run_id,
            submission_id,
            input,
            run_config,
            config_revision,
        } => {
            if state.lifecycle.status != CoreAgentStatus::Open {
                return Err(DomainError::InvariantViolation(
                    "runs can only be accepted while session is open".into(),
                ));
            }
            if *config_revision != state.lifecycle.config_revision {
                return Err(DomainError::InvariantViolation(format!(
                    "accepted run config revision {} does not match session revision {}",
                    config_revision, state.lifecycle.config_revision
                )));
            }
            crate::core::components::config::validate_run_config_for_state(state, run_config)?;
            let expected_run_id =
                state.id_cursors.last_run_id.checked_add(1).ok_or_else(|| {
                    DomainError::InvariantViolation("run id cursor exhausted".into())
                })?;
            if run_id.as_u64() != expected_run_id {
                return Err(DomainError::InvariantViolation(format!(
                    "expected run id {}, got {}",
                    expected_run_id, run_id
                )));
            }
            state.runs.queued.push(AcceptedRun {
                run_id: *run_id,
                submission_id: submission_id.clone(),
                input: input.clone(),
                run_config: run_config.clone(),
                config_revision: *config_revision,
            });
            state.id_cursors.last_run_id = run_id.as_u64();
            Ok(())
        }
        Event::Started { run_id } => {
            if state.lifecycle.status != CoreAgentStatus::Open {
                return Err(DomainError::InvariantViolation(
                    "runs can only start while session is open".into(),
                ));
            }
            if state.runs.active.is_some() {
                return Err(DomainError::InvariantViolation(
                    "cannot start run while another run is active".into(),
                ));
            }

            let Some(queued) = state.runs.queued.first().cloned() else {
                return Err(DomainError::InvariantViolation(
                    "cannot start run without queued work".into(),
                ));
            };
            if queued.run_id != *run_id {
                return Err(DomainError::InvariantViolation(
                    "started run does not match first queued run".into(),
                ));
            }

            state.runs.queued.remove(0);
            state.runs.active = Some(ActiveRun {
                run_id: *run_id,
                status: RunStatus::Active,
                submission_id: queued.submission_id,
                input: queued.input,
                run_config: queued.run_config,
                config_revision: queued.config_revision,
                steering: Vec::new(),
                turns: BTreeMap::new(),
                active_turn_id: None,
                active_tool_batch_id: None,
                tool_batches: BTreeMap::new(),
                completed_tool_batches: BTreeMap::new(),
                output_ref: None,
                failure: None,
            });
            Ok(())
        }
        Event::SteeringAccepted { run_id, input } => {
            let active_run = active_run_mut(state, *run_id)?;
            if active_run.status != RunStatus::Active {
                return Err(DomainError::InvariantViolation(
                    "steering can only be added to active runs".into(),
                ));
            }
            active_run.steering.push(input.clone());
            Ok(())
        }
        Event::CancellationRequested { run_id } => {
            let active_run = active_run_mut(state, *run_id)?;
            if active_run.status != RunStatus::Active {
                return Err(DomainError::InvariantViolation(
                    "only active runs can request cancellation".into(),
                ));
            }
            active_run.status = RunStatus::Cancelling;
            Ok(())
        }
        Event::Completed { run_id, output_ref } => finish_active_run(
            state,
            *run_id,
            RunStatus::Completed,
            output_ref.clone(),
            None,
        ),
        Event::Failed { run_id, failure } => finish_active_run(
            state,
            *run_id,
            RunStatus::Failed,
            None,
            Some(failure.clone()),
        ),
        Event::Cancelled { run_id } => {
            let Some(active_run) = state.runs.active.as_ref() else {
                return Err(DomainError::InvariantViolation("no active run".into()));
            };
            if active_run.status != RunStatus::Cancelling {
                return Err(DomainError::InvariantViolation(
                    "only cancelling runs can become cancelled".into(),
                ));
            }
            finish_active_run(state, *run_id, RunStatus::Cancelled, None, None)
        }
    }
}

pub(crate) fn active_run_mut(
    state: &mut CoreAgentState,
    run_id: RunId,
) -> Result<&mut ActiveRun, DomainError> {
    let Some(active_run) = state.runs.active.as_mut() else {
        return Err(DomainError::InvariantViolation("no active run".into()));
    };
    if active_run.run_id != run_id {
        return Err(DomainError::InvariantViolation(format!(
            "event run id {} does not match active run {}",
            run_id, active_run.run_id
        )));
    }
    Ok(active_run)
}

pub(crate) fn active_run_ref(
    state: &CoreAgentState,
    run_id: RunId,
) -> Result<&ActiveRun, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Err(DomainError::InvariantViolation("no active run".into()));
    };
    if active_run.run_id != run_id {
        return Err(DomainError::InvariantViolation(format!(
            "event run id {} does not match active run {}",
            run_id, active_run.run_id
        )));
    }
    Ok(active_run)
}

fn finish_active_run(
    state: &mut CoreAgentState,
    run_id: RunId,
    status: RunStatus,
    output_ref: Option<BlobRef>,
    failure: Option<RunFailure>,
) -> Result<(), DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Err(DomainError::InvariantViolation("no active run".into()));
    };
    if active_run.run_id != run_id {
        return Err(DomainError::InvariantViolation(format!(
            "event run id {} does not match active run {}",
            run_id, active_run.run_id
        )));
    }
    let active_run = state
        .runs
        .active
        .take()
        .expect("active run checked before take");
    state.runs.completed.push(RunRecord {
        run_id: active_run.run_id,
        status,
        submission_id: active_run.submission_id,
        output_ref,
        failure,
    });
    state
        .skills
        .activations
        .retain(|activation| activation.scope != SkillActivationScope::Run);
    Ok(())
}
