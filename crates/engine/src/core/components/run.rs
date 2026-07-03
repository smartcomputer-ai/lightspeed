use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActiveToolBatch, BlobRef, CompletedToolBatch, ContextEntryId, ContextEntryInput,
    ContextEntryKey, CoreAgentEventKind, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, PlanningError, RunConfig, RunId, SteeringId, SubmissionId,
    ToolBatchId, TurnId, TurnOutcome, TurnState, TurnStatus,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Accepted(AcceptedRunEvent),
    Started {
        run_id: RunId,
    },
    SteeringAccepted {
        run_id: RunId,
        steering_id: SteeringId,
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
    pub source: RunSource,
    pub run_config: RunConfig,
    pub config_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringBatch {
    pub steering_id: SteeringId,
    pub input: Vec<ContextEntryInput>,
    pub entry_ids: Vec<ContextEntryId>,
    pub consumed_by_turn_id: Option<TurnId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRun {
    pub run_id: RunId,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    pub source: RunSource,
    pub input_entry_ids: Vec<ContextEntryId>,
    pub input_consumed_by_turn_id: Option<TurnId>,
    pub run_config: RunConfig,
    pub config_revision: u64,
    pub steering: Vec<SteeringBatch>,
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
    /// Digest of the accepted (input, run_config), kept after the full input
    /// is dropped so duplicate submissions can still be checked for
    /// input/config equality against completed runs.
    #[serde(default)]
    pub submission_digest: Option<u64>,
    pub output_ref: Option<BlobRef>,
    pub failure: Option<RunFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    pub kind: RunFailureKind,
    pub message_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedRunEvent {
    pub run_id: RunId,
    pub submission_id: Option<SubmissionId>,
    pub source: RunSource,
    pub run_config: RunConfig,
    pub config_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequestCommand {
    pub submission_id: Option<SubmissionId>,
    pub source: RunRequestSource,
    pub run_config: RunConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RunRequestSource {
    Input { input: Vec<ContextEntryInput> },
    Context { keys: Vec<ContextEntryKey> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSourceContextTrigger {
    pub key: ContextEntryKey,
    pub entry_id: ContextEntryId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RunSource {
    Input {
        input: Vec<ContextEntryInput>,
    },
    Context {
        triggers: Vec<RunSourceContextTrigger>,
    },
}

impl RunRequestSource {
    pub fn input(&self) -> &[ContextEntryInput] {
        match self {
            Self::Input { input } => input,
            Self::Context { .. } => &[],
        }
    }

    pub fn context_keys(&self) -> &[ContextEntryKey] {
        match self {
            Self::Input { .. } => &[],
            Self::Context { keys } => keys,
        }
    }
}

impl RunSource {
    pub fn input(&self) -> &[ContextEntryInput] {
        match self {
            Self::Input { input } => input,
            Self::Context { .. } => &[],
        }
    }

    pub fn context_triggers(&self) -> &[RunSourceContextTrigger] {
        match self {
            Self::Input { .. } => &[],
            Self::Context { triggers } => triggers,
        }
    }

    pub fn context_keys(&self) -> Vec<ContextEntryKey> {
        match self {
            Self::Input { .. } => Vec::new(),
            Self::Context { triggers } => {
                triggers.iter().map(|trigger| trigger.key.clone()).collect()
            }
        }
    }

    /// Whether this accepted source matches a client-requested source.
    /// Context sources compare by trigger keys; resolved entry ids are an
    /// admission-time snapshot, not part of the request identity.
    pub fn matches_request(&self, request: &RunRequestSource) -> bool {
        match (self, request) {
            (Self::Input { input }, RunRequestSource::Input { input: requested }) => {
                input == requested
            }
            (Self::Context { triggers }, RunRequestSource::Context { keys: requested }) => triggers
                .iter()
                .map(|trigger| &trigger.key)
                .eq(requested.iter()),
            _ => false,
        }
    }
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

pub fn plan_next(state: &CoreAgentState) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
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
        Event::Accepted(accepted) => {
            let AcceptedRunEvent {
                run_id,
                submission_id,
                source,
                run_config,
                config_revision,
            } = accepted;
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
                source: source.clone(),
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
                source: queued.source,
                input_entry_ids: Vec::new(),
                input_consumed_by_turn_id: None,
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
        Event::SteeringAccepted {
            run_id,
            steering_id,
            input,
        } => {
            let expected_steering_id = state
                .id_cursors
                .last_steering_id
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::InvariantViolation("steering id cursor exhausted".into())
                })?;
            if steering_id.as_u64() != expected_steering_id {
                return Err(DomainError::InvariantViolation(format!(
                    "expected steering id {}, got {}",
                    expected_steering_id, steering_id
                )));
            }
            let active_run = active_run_mut(state, *run_id)?;
            if active_run.status != RunStatus::Active {
                return Err(DomainError::InvariantViolation(
                    "steering can only be added to active runs".into(),
                ));
            }
            active_run.steering.push(SteeringBatch {
                steering_id: *steering_id,
                input: input.clone(),
                entry_ids: Vec::new(),
                consumed_by_turn_id: None,
            });
            state.id_cursors.last_steering_id = steering_id.as_u64();
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

/// Outcome of matching a `RequestRun` submission id against runs already in
/// session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmissionMatch {
    /// Same submission id with the same input and run config: the command is
    /// a retry and admits as an idempotent no-op.
    Identical,
    /// Same submission id but different input or run config: a client bug,
    /// rejected.
    Different,
}

pub(crate) fn match_existing_submission(
    state: &CoreAgentState,
    submission_id: &SubmissionId,
    source: &RunRequestSource,
    run_config: &RunConfig,
) -> Option<SubmissionMatch> {
    if let Some(active) = state
        .runs
        .active
        .as_ref()
        .filter(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        return Some(
            if active.source.matches_request(source) && &active.run_config == run_config {
                SubmissionMatch::Identical
            } else {
                SubmissionMatch::Different
            },
        );
    }
    if let Some(queued) = state
        .runs
        .queued
        .iter()
        .find(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        return Some(
            if queued.source.matches_request(source) && &queued.run_config == run_config {
                SubmissionMatch::Identical
            } else {
                SubmissionMatch::Different
            },
        );
    }
    if let Some(completed) = state
        .runs
        .completed
        .iter()
        .find(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        // Completed runs keep only a digest of their accepted source/config.
        // A record without a digest cannot be compared and is treated as a
        // retry, which is the safe behavior for retried clients.
        return Some(match completed.submission_digest {
            Some(digest) if digest != run_submission_digest(source, run_config) => {
                SubmissionMatch::Different
            }
            _ => SubmissionMatch::Identical,
        });
    }
    None
}

/// Deterministic digest of a run submission's payload. FNV-1a over the
/// serde_json encoding; collision resistance is not a goal — this guards
/// against client bugs, not adversaries.
pub fn run_submission_digest(source: &RunRequestSource, run_config: &RunConfig) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let bytes = serde_json::to_vec(&(source, run_config))
        .expect("run submission payload serializes to JSON");
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub(crate) fn source_request_equivalent(source: &RunSource) -> RunRequestSource {
    match source {
        RunSource::Input { input } => RunRequestSource::Input {
            input: input.clone(),
        },
        RunSource::Context { triggers } => RunRequestSource::Context {
            keys: triggers.iter().map(|trigger| trigger.key.clone()).collect(),
        },
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
    let submission_digest = active_run.submission_id.as_ref().map(|_| {
        run_submission_digest(
            &source_request_equivalent(&active_run.source),
            &active_run.run_config,
        )
    });
    state.runs.completed.push(RunRecord {
        run_id: active_run.run_id,
        status,
        submission_id: active_run.submission_id,
        submission_digest,
        output_ref,
        failure,
    });
    crate::core::components::context::expire_run_scoped_context_entries(state)?;
    Ok(())
}
