use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActiveToolBatch, ApprovalId, ApprovalRecord, ApprovalStatus, BlobRef, CompletedToolBatch,
    ContextEntryId, ContextEntryInput, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins,
    CoreAgentState, CoreAgentStatus, DomainError, EventSeq, LlmUsage, PlanningError, PromiseId,
    RunConfig, RunId, SteeringId, SubmissionId, ToolBatchId, ToolCallId, TurnId, TurnOutcome,
    TurnState, TurnStatus,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// Accepted run payloads intentionally keep their durable event shape inline.
#[allow(clippy::large_enum_variant)]
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
        output: Option<crate::ContentRef>,
    },
    Failed {
        run_id: RunId,
        failure: RunFailure,
    },
    Cancelled {
        run_id: RunId,
    },
    /// Forced terminal transition for a wedged run: unlike `Cancelled`, this
    /// applies from any non-terminal status and does not require open turns
    /// or tool batches to be drained first. Emitted by watchdog/force-close
    /// recovery paths, never by normal planning.
    ForceCancelled {
        run_id: RunId,
    },
    /// A queued run removed without ever starting (force-close drains the
    /// queue). Recorded as a `Cancelled` run record.
    QueuedCancelled {
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
    /// Durable event-log position and wall-clock time of acceptance. These
    /// metadata facts let current-state readers page runs without replaying
    /// their event ranges.
    pub first_seq: EventSeq,
    pub accepted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
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
    pub first_seq: EventSeq,
    pub accepted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Provider usage accumulated as generation-completed events apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    pub steering: Vec<SteeringBatch>,
    pub turns: BTreeMap<TurnId, TurnState>,
    pub active_turn_id: Option<TurnId>,
    pub active_tool_batch_id: Option<ToolBatchId>,
    /// Single-use approval records owned by this run. They disappear from
    /// materialized state with the active run; their immutable audit trail
    /// remains in the session event log.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub approvals: BTreeMap<ApprovalId, ApprovalRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_tool_batch: Option<ParkedToolBatch>,
    pub tool_batches: BTreeMap<ToolBatchId, ActiveToolBatch>,
    pub completed_tool_batches: BTreeMap<ToolBatchId, CompletedToolBatch>,
    pub output: Option<crate::ContentRef>,
    pub failure: Option<RunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

impl ActiveRun {
    pub fn pending_approvals(&self) -> impl Iterator<Item = &ApprovalRecord> {
        self.approvals
            .values()
            .filter(|record| record.status == ApprovalStatus::Pending)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub enum RunStatus {
    Active,
    Parked,
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
    /// Digest of the accepted submission payload, kept after the full input is
    /// dropped so duplicate submissions can still be checked for equality
    /// against completed runs.
    #[serde(default)]
    pub submission_digest: Option<u64>,
    /// Accepted source retained as scalar metadata and CAS references only;
    /// resolved content never enters reducer state.
    pub source: RunSource,
    pub first_seq: EventSeq,
    pub terminal_seq: EventSeq,
    pub accepted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    pub output: Option<crate::ContentRef>,
    pub failure: Option<RunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promise_ids: Vec<PromiseId>,
    pub mode: AwaitMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitMode {
    #[default]
    All,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedToolBatch {
    pub batch_id: ToolBatchId,
    pub suspension: ToolBatchSuspension,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolBatchSuspension {
    AwaitTool {
        call_id: ToolCallId,
        spec: AwaitSpec,
    },
    JoinedWorkflowCalls {
        calls: Vec<JoinedWorkflowCall>,
        spec: AwaitSpec,
    },
}

impl ToolBatchSuspension {
    pub fn spec(&self) -> &AwaitSpec {
        match self {
            Self::AwaitTool { spec, .. } | Self::JoinedWorkflowCalls { spec, .. } => spec,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinedWorkflowCall {
    pub call_id: ToolCallId,
    pub invocation_id: crate::WorkflowToolInvocationId,
    pub promise_id: PromiseId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeReason {
    Cancelled,
    Timeout,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeToolBatchCommand {
    pub run_id: RunId,
    pub batch_id: ToolBatchId,
    pub claim: WakeReason,
    pub claim_observed_at_ms: u64,
    pub output: ToolBatchResumeOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolBatchResumeOutput {
    AwaitTool {
        result_ref: BlobRef,
        /// Context the awaited results supply beyond the combined result
        /// itself, appended after it in this order. Prepared by the runtime,
        /// which reads the payloads; the reducer only validates and records.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_context: Vec<crate::ContextEntryInput>,
    },
    JoinedWorkflowCalls {
        /// Context per joined promise, appended after that promise's call
        /// result. Joined calls complete separately, so supplements keep
        /// their promise association.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_context: Vec<PromiseContextEntries>,
    },
}

/// Model-visible entries one resolved promise supplies beside its result,
/// the same shape ordinary tool results carry in
/// `model_visible_context_entries`. What they contain is the runtime's
/// business; the reducer owns promise status, call association, and order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseContextEntries {
    pub promise_id: PromiseId,
    pub entries: Vec<crate::ContextEntryInput>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequestCommand {
    pub submission_id: Option<SubmissionId>,
    pub source: RunRequestSource,
    pub run_config: RunConfig,
    /// Cross-session notify-intents recorded at admission: on this run's
    /// terminal event, signal each holder workflow with its token (the
    /// holder-side promise id). The edge event is the subscription.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

/// One log-backed notify-intent attached to a run. Replaces the former
/// `subscribe_run` machinery: recorded by the event that creates the edge
/// (spawn admission), rebuilt with the run at bootstrap, including after the
/// observed session's own continue-as-new.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTerminalNotifyIntent {
    pub holder_workflow_id: String,
    pub token: String,
}

/// Every run starts from explicit input entries. The tagged single-variant
/// encoding keeps the wire shape (`{"type":"input",...}`) stable for any
/// future source kind without a migration of stored accepted events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RunRequestSource {
    Input { input: Vec<ContextEntryInput> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RunSource {
    Input { input: Vec<ContextEntryInput> },
}

impl RunRequestSource {
    pub fn input(&self) -> &[ContextEntryInput] {
        match self {
            Self::Input { input } => input,
        }
    }
}

impl RunSource {
    pub fn input(&self) -> &[ContextEntryInput] {
        match self {
            Self::Input { input } => input,
        }
    }

    /// Whether this accepted source matches a client-requested source.
    pub fn matches_request(&self, request: &RunRequestSource) -> bool {
        match (self, request) {
            (Self::Input { input }, RunRequestSource::Input { input: requested }) => {
                input == requested
            }
        }
    }

    pub fn matches_message_input(&self, requested: &[ContextEntryInput]) -> bool {
        match self {
            Self::Input { input } => input == requested,
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
            // A cancelling run whose in-flight work has drained is terminal
            // at once: client cancels never fan out a farewell LLM turn. A
            // completed tool-call turn whose batch has not started yet is
            // left to the tooling planner first, so its calls get cancelled
            // results before the run ends.
            if active_run.status == RunStatus::Cancelling
                && !has_unstarted_tool_call_turn(active_run)
            {
                let joins = CoreAgentJoins {
                    run_id: Some(active_run.run_id),
                    ..CoreAgentJoins::default()
                };
                return Ok(vec![CoreAgentEventProposal::new(
                    joins,
                    CoreAgentEvent::Run(Event::Cancelled {
                        run_id: active_run.run_id,
                    }),
                )]);
            }
            if active_run.status == RunStatus::Active
                && let Some(proposal) = terminal_run_proposal(active_run)?
            {
                return Ok(vec![proposal]);
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
    let kind = CoreAgentEvent::Run(Event::Started {
        run_id: queued.run_id,
    });

    Ok(vec![CoreAgentEventProposal::new(joins, kind)])
}

/// Steering the model has not seen yet: a batch no turn has consumed. A
/// final-output turn does not end a run while such steering exists — the
/// run gets one more turn that carries it, so "steer" never silently does
/// nothing on a single-turn answer.
pub(crate) fn has_unconsumed_steering(active_run: &ActiveRun) -> bool {
    active_run
        .steering
        .iter()
        .any(|steering| steering.consumed_by_turn_id.is_none())
}

fn terminal_run_proposal(
    active_run: &ActiveRun,
) -> Result<Option<CoreAgentEventProposal>, PlanningError> {
    let Some((turn_id, turn)) = active_run.turns.iter().next_back() else {
        return Ok(None);
    };
    let kind = match (&turn.status, turn.outcome.as_ref()) {
        (TurnStatus::Completed, Some(TurnOutcome::FinalOutput { .. }))
            if has_unconsumed_steering(active_run) =>
        {
            None
        }
        (TurnStatus::Completed, Some(TurnOutcome::FinalOutput { output })) => {
            Some(CoreAgentEvent::Run(Event::Completed {
                run_id: active_run.run_id,
                output: output.clone(),
            }))
        }
        (TurnStatus::Failed, Some(TurnOutcome::Failed { failure_ref })) => {
            Some(CoreAgentEvent::Run(Event::Failed {
                run_id: active_run.run_id,
                failure: RunFailure {
                    kind: RunFailureKind::ModelFailure,
                    message_ref: failure_ref.clone(),
                },
            }))
        }
        (TurnStatus::Cancelled, Some(TurnOutcome::Cancelled)) => {
            Some(CoreAgentEvent::Run(Event::Failed {
                run_id: active_run.run_id,
                failure: RunFailure {
                    kind: RunFailureKind::Cancelled,
                    message_ref: None,
                },
            }))
        }
        (
            TurnStatus::Completed,
            Some(
                TurnOutcome::ToolCallsQueued
                | TurnOutcome::ContextUpdateRequired
                | TurnOutcome::ApprovalsRequested,
            ),
        ) => None,
        (
            TurnStatus::Failed | TurnStatus::Cancelled,
            Some(
                TurnOutcome::ToolCallsQueued
                | TurnOutcome::ContextUpdateRequired
                | TurnOutcome::ApprovalsRequested,
            ),
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

/// True when a completed tool-call turn has no tool batch yet (active or
/// completed); the tooling planner owns the next step for that run.
fn has_unstarted_tool_call_turn(active_run: &ActiveRun) -> bool {
    active_run.turns.iter().any(|(turn_id, turn)| {
        turn.status == TurnStatus::Completed
            && turn.outcome == Some(TurnOutcome::ToolCallsQueued)
            && turn
                .facts
                .as_ref()
                .is_some_and(|facts| !facts.tool_calls.is_empty())
            && !active_run
                .tool_batches
                .values()
                .any(|batch| batch.turn_id == *turn_id)
            && !active_run
                .completed_tool_batches
                .values()
                .any(|batch| batch.turn_id == *turn_id)
    })
}

pub(crate) fn latest_turn_is_terminal_run_outcome(
    active_run: &ActiveRun,
) -> Result<bool, PlanningError> {
    Ok(terminal_run_proposal(active_run)?.is_some())
}

pub(crate) fn apply_event(
    state: &mut CoreAgentState,
    event: &Event,
    seq: EventSeq,
    observed_at_ms: u64,
) -> Result<(), DomainError> {
    match event {
        Event::Accepted(accepted) => {
            let AcceptedRunEvent {
                run_id,
                submission_id,
                source,
                run_config,
                config_revision,
                notify_on_terminal,
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
                first_seq: seq,
                accepted_at_ms: observed_at_ms,
                notify_on_terminal: notify_on_terminal.clone(),
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
                notify_on_terminal: queued.notify_on_terminal,
                input_entry_ids: Vec::new(),
                input_consumed_by_turn_id: None,
                run_config: queued.run_config,
                config_revision: queued.config_revision,
                first_seq: queued.first_seq,
                accepted_at_ms: queued.accepted_at_ms,
                started_at_ms: Some(observed_at_ms),
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
            if !matches!(active_run.status, RunStatus::Active | RunStatus::Parked) {
                return Err(DomainError::InvariantViolation(
                    "steering can only be added to active or parked runs".into(),
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
            if !matches!(active_run.status, RunStatus::Active | RunStatus::Parked) {
                return Err(DomainError::InvariantViolation(
                    "only active or parked runs can request cancellation".into(),
                ));
            }
            active_run.status = RunStatus::Cancelling;
            Ok(())
        }
        Event::Completed { run_id, output } => finish_active_run(
            state,
            *run_id,
            RunStatus::Completed,
            output.clone(),
            None,
            seq,
            observed_at_ms,
        ),
        Event::Failed { run_id, failure } => finish_active_run(
            state,
            *run_id,
            RunStatus::Failed,
            None,
            Some(failure.clone()),
            seq,
            observed_at_ms,
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
            if active_run.active_turn_id.is_some() || active_run.active_tool_batch_id.is_some() {
                return Err(DomainError::InvariantViolation(
                    "cancellation requires drained active work".into(),
                ));
            }
            finish_active_run(
                state,
                *run_id,
                RunStatus::Cancelled,
                None,
                None,
                seq,
                observed_at_ms,
            )
        }
        Event::ForceCancelled { run_id } => {
            let Some(active_run) = state.runs.active.as_ref() else {
                return Err(DomainError::InvariantViolation("no active run".into()));
            };
            if !matches!(
                active_run.status,
                RunStatus::Active | RunStatus::Parked | RunStatus::Cancelling
            ) {
                return Err(DomainError::InvariantViolation(
                    "only non-terminal runs can be force-cancelled".into(),
                ));
            }
            finish_active_run(
                state,
                *run_id,
                RunStatus::Cancelled,
                None,
                None,
                seq,
                observed_at_ms,
            )
        }
        Event::QueuedCancelled { run_id } => {
            let Some(position) = state
                .runs
                .queued
                .iter()
                .position(|queued| queued.run_id == *run_id)
            else {
                return Err(DomainError::InvariantViolation(format!(
                    "queued run {} not found",
                    run_id
                )));
            };
            let queued = state.runs.queued.remove(position);
            let submission_digest = queued
                .submission_id
                .as_ref()
                .map(|_| submission_digest_for_accepted_run(&queued));
            state.runs.completed.push(RunRecord {
                run_id: queued.run_id,
                status: RunStatus::Cancelled,
                submission_id: queued.submission_id,
                submission_digest,
                source: compact_terminal_source(queued.source),
                first_seq: queued.first_seq,
                terminal_seq: seq,
                accepted_at_ms: queued.accepted_at_ms,
                started_at_ms: None,
                completed_at_ms: observed_at_ms,
                usage: None,
                output: None,
                failure: None,
                notify_on_terminal: queued.notify_on_terminal,
            });
            Ok(())
        }
    }
}

/// Outcome of matching a submission id against runs or consumed messages
/// already in session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmissionMatch {
    /// Same submission id with the same command kind and payload: the command
    /// is a retry and admits as an idempotent no-op.
    Identical,
    /// Same submission id but different command kind or payload: a client
    /// bug, rejected.
    Different,
}

pub(crate) fn match_existing_run_submission(
    state: &CoreAgentState,
    submission_id: &SubmissionId,
    source: &RunRequestSource,
    run_config: &RunConfig,
    notify_on_terminal: &[RunTerminalNotifyIntent],
) -> Option<SubmissionMatch> {
    if let Some(active) = state
        .runs
        .active
        .as_ref()
        .filter(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        return Some(
            if active.source.matches_request(source)
                && &active.run_config == run_config
                && active.notify_on_terminal == notify_on_terminal
            {
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
            if queued.source.matches_request(source)
                && &queued.run_config == run_config
                && queued.notify_on_terminal == notify_on_terminal
            {
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
            Some(digest)
                if digest
                    != request_run_submission_digest(source, run_config, notify_on_terminal) =>
            {
                SubmissionMatch::Different
            }
            _ => SubmissionMatch::Identical,
        });
    }
    None
}

/// Deterministic digest of a request-run submission's payload. FNV-1a over
/// the serde_json encoding; collision resistance is not a goal — this guards
/// against client bugs, not adversaries.
pub fn request_run_submission_digest(
    source: &RunRequestSource,
    run_config: &RunConfig,
    notify_on_terminal: &[RunTerminalNotifyIntent],
) -> u64 {
    submission_digest_json(&("request_run", source, run_config, notify_on_terminal))
}

fn submission_digest_json<T: Serialize>(payload: &T) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let bytes = serde_json::to_vec(payload).expect("submission payload serializes to JSON");
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn submission_digest_for_accepted_run(run: &AcceptedRun) -> u64 {
    request_run_submission_digest(
        &source_request_equivalent(&run.source),
        &run.run_config,
        &run.notify_on_terminal,
    )
}

pub(crate) fn source_request_equivalent(source: &RunSource) -> RunRequestSource {
    match source {
        RunSource::Input { input } => RunRequestSource::Input {
            input: input.clone(),
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
    output: Option<crate::ContentRef>,
    failure: Option<RunFailure>,
    terminal_seq: EventSeq,
    completed_at_ms: u64,
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
        let accepted = AcceptedRun {
            run_id: active_run.run_id,
            submission_id: active_run.submission_id.clone(),
            source: active_run.source.clone(),
            run_config: active_run.run_config.clone(),
            config_revision: active_run.config_revision,
            first_seq: active_run.first_seq,
            accepted_at_ms: active_run.accepted_at_ms,
            notify_on_terminal: active_run.notify_on_terminal.clone(),
        };
        submission_digest_for_accepted_run(&accepted)
    });
    state.runs.completed.push(RunRecord {
        run_id: active_run.run_id,
        status,
        submission_id: active_run.submission_id,
        submission_digest,
        source: compact_terminal_source(active_run.source),
        first_seq: active_run.first_seq,
        terminal_seq,
        accepted_at_ms: active_run.accepted_at_ms,
        started_at_ms: active_run.started_at_ms,
        completed_at_ms,
        usage: active_run.usage,
        output,
        failure,
        notify_on_terminal: active_run.notify_on_terminal,
    });
    Ok(())
}

fn compact_terminal_source(mut source: RunSource) -> RunSource {
    let RunSource::Input { input } = &mut source;
    for entry in input {
        entry.preview = None;
    }
    source
}
