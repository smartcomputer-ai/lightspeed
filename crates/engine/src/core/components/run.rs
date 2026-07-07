use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    ActiveToolBatch, BlobRef, CompletedToolBatch, ContextEntryId, ContextEntryInput,
    ContextEntryKey, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, MessageId, PlanningError, PromiseId, RunConfig, RunId,
    SteeringId, SubmissionId, ToolBatchId, ToolCallId, TurnId, TurnOutcome, TurnState, TurnStatus,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Accepted(AcceptedRunEvent),
    Started {
        run_id: RunId,
    },
    MessageBuffered {
        message_id: MessageId,
        submission_id: Option<SubmissionId>,
        submission_digest: u64,
        input: Vec<ContextEntryInput>,
        run_config: RunConfig,
        config_revision: u64,
    },
    MessageConsumedByAwait {
        message_id: MessageId,
        run_id: RunId,
    },
    MessagePromotedToRun {
        message_id: MessageId,
        run_id: RunId,
    },
    MessageCancelled {
        message_id: MessageId,
    },
    SteeringAccepted {
        run_id: RunId,
        steering_id: SteeringId,
        input: Vec<ContextEntryInput>,
    },
    CancellationRequested {
        run_id: RunId,
    },
    CancellationGraceStarted {
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
    #[serde(default)]
    pub origin: RunOrigin,
    pub source: RunSource,
    pub run_config: RunConfig,
    pub config_revision: u64,
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
    #[serde(default)]
    pub origin: RunOrigin,
    pub source: RunSource,
    pub input_entry_ids: Vec<ContextEntryId>,
    pub input_consumed_by_turn_id: Option<TurnId>,
    pub run_config: RunConfig,
    pub config_revision: u64,
    pub steering: Vec<SteeringBatch>,
    pub turns: BTreeMap<TurnId, TurnState>,
    pub active_turn_id: Option<TurnId>,
    pub active_tool_batch_id: Option<ToolBatchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_await: Option<ParkedAwait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_grace_turn_id: Option<TurnId>,
    pub tool_batches: BTreeMap<ToolBatchId, ActiveToolBatch>,
    pub completed_tool_batches: BTreeMap<ToolBatchId, CompletedToolBatch>,
    pub output_ref: Option<BlobRef>,
    pub failure: Option<RunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Parked,
    Cancelling,
    CancellingGrace,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    #[serde(default)]
    pub origin: RunOrigin,
    /// Digest of the accepted submission payload, kept after the full input is
    /// dropped so duplicate submissions can still be checked for equality
    /// against completed runs.
    #[serde(default)]
    pub submission_digest: Option<u64>,
    pub output_ref: Option<BlobRef>,
    pub failure: Option<RunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferedMessage {
    pub message_id: MessageId,
    pub submission_id: Option<SubmissionId>,
    pub submission_digest: u64,
    pub input: Vec<ContextEntryInput>,
    pub run_config: RunConfig,
    pub config_revision: u64,
    pub status: MessageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_by_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_to_run_id: Option<RunId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Buffered,
    ConsumedByAwait,
    PromotedToRun,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promise_ids: Vec<PromiseId>,
    pub mode: AwaitMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mailbox: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitMode {
    #[default]
    All,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedAwait {
    pub batch_id: ToolBatchId,
    pub call_id: ToolCallId,
    pub spec: AwaitSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeReason {
    Cancelled,
    MailboxMessage,
    Timeout,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitOutputRefs {
    pub output_ref: BlobRef,
    pub summary_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeAwaitCommand {
    pub run_id: RunId,
    pub batch_id: ToolBatchId,
    pub claim: WakeReason,
    pub claim_observed_at_ms: u64,
    pub output: AwaitOutputRefs,
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
    #[serde(default)]
    pub origin: RunOrigin,
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
    /// holder-side promise id). The edge event is the subscription (P92 §1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_on_terminal: Vec<RunTerminalNotifyIntent>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOrigin {
    #[default]
    Requested,
    Message,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitMessageCommand {
    pub submission_id: Option<SubmissionId>,
    pub input: Vec<ContextEntryInput>,
}

/// One log-backed notify-intent attached to a run. Replaces the P84
/// `subscribe_run` machinery: recorded by the event that creates the edge
/// (spawn admission), rebuilt with the run at bootstrap, including after the
/// observed session's own continue-as-new.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTerminalNotifyIntent {
    pub holder_workflow_id: String,
    pub token: String,
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

    pub fn matches_message_input(&self, requested: &[ContextEntryInput]) -> bool {
        match self {
            Self::Input { input } => input == requested,
            Self::Context { .. } => false,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<BufferedMessage>,
}

fn is_false(value: &bool) -> bool {
    !*value
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
                    CoreAgentEvent::Run(Event::CancellationGraceStarted {
                        run_id: active_run.run_id,
                    }),
                )]);
            }
            if active_run.status == RunStatus::CancellingGrace
                && active_run.cancellation_grace_turn_id.is_some()
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
    let kind = CoreAgentEvent::Run(Event::Started {
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
            Some(CoreAgentEvent::Run(Event::Completed {
                run_id: active_run.run_id,
                output_ref: output_ref.clone(),
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
                origin,
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
                origin: *origin,
                source: source.clone(),
                run_config: run_config.clone(),
                config_revision: *config_revision,
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
                origin: queued.origin,
                source: queued.source,
                notify_on_terminal: queued.notify_on_terminal,
                input_entry_ids: Vec::new(),
                input_consumed_by_turn_id: None,
                run_config: queued.run_config,
                config_revision: queued.config_revision,
                steering: Vec::new(),
                turns: BTreeMap::new(),
                active_turn_id: None,
                active_tool_batch_id: None,
                parked_await: None,
                cancellation_grace_turn_id: None,
                tool_batches: BTreeMap::new(),
                completed_tool_batches: BTreeMap::new(),
                output_ref: None,
                failure: None,
            });
            Ok(())
        }
        Event::MessageBuffered {
            message_id,
            submission_id,
            submission_digest,
            input,
            run_config,
            config_revision,
        } => {
            if state.lifecycle.status != CoreAgentStatus::Open {
                return Err(DomainError::InvariantViolation(
                    "messages can only be buffered while session is open".into(),
                ));
            }
            let expected_message_id =
                state
                    .id_cursors
                    .last_message_id
                    .checked_add(1)
                    .ok_or_else(|| {
                        DomainError::InvariantViolation("message id cursor exhausted".into())
                    })?;
            if message_id.as_u64() != expected_message_id {
                return Err(DomainError::InvariantViolation(format!(
                    "expected message id {}, got {}",
                    expected_message_id, message_id
                )));
            }
            if *config_revision != state.lifecycle.config_revision {
                return Err(DomainError::InvariantViolation(format!(
                    "buffered message config revision {} does not match session revision {}",
                    config_revision, state.lifecycle.config_revision
                )));
            }
            crate::core::components::config::validate_run_config_for_state(state, run_config)?;
            state.runs.messages.push(BufferedMessage {
                message_id: *message_id,
                submission_id: submission_id.clone(),
                submission_digest: *submission_digest,
                input: input.clone(),
                run_config: run_config.clone(),
                config_revision: *config_revision,
                status: MessageStatus::Buffered,
                consumed_by_run_id: None,
                promoted_to_run_id: None,
            });
            state.id_cursors.last_message_id = message_id.as_u64();
            Ok(())
        }
        Event::MessageConsumedByAwait { message_id, run_id } => {
            active_run_ref(state, *run_id)?;
            let message = message_mut(state, *message_id)?;
            if message.status != MessageStatus::Buffered {
                return Err(DomainError::InvariantViolation(format!(
                    "message {} is not buffered",
                    message_id
                )));
            }
            message.status = MessageStatus::ConsumedByAwait;
            message.consumed_by_run_id = Some(*run_id);
            Ok(())
        }
        Event::MessagePromotedToRun { message_id, run_id } => {
            if !state.runs.queued.iter().any(|run| run.run_id == *run_id)
                && !state
                    .runs
                    .active
                    .as_ref()
                    .is_some_and(|run| run.run_id == *run_id)
                && !state.runs.completed.iter().any(|run| run.run_id == *run_id)
            {
                return Err(DomainError::InvariantViolation(format!(
                    "promoted message {} references missing run {}",
                    message_id, run_id
                )));
            }
            let message = message_mut(state, *message_id)?;
            if message.status != MessageStatus::Buffered {
                return Err(DomainError::InvariantViolation(format!(
                    "message {} is not buffered",
                    message_id
                )));
            }
            message.status = MessageStatus::PromotedToRun;
            message.promoted_to_run_id = Some(*run_id);
            Ok(())
        }
        Event::MessageCancelled { message_id } => {
            let message = message_mut(state, *message_id)?;
            if message.status != MessageStatus::Buffered {
                return Err(DomainError::InvariantViolation(format!(
                    "message {} is not buffered",
                    message_id
                )));
            }
            message.status = MessageStatus::Cancelled;
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
            if !matches!(active_run.status, RunStatus::Active | RunStatus::Parked) {
                return Err(DomainError::InvariantViolation(
                    "only active or parked runs can request cancellation".into(),
                ));
            }
            active_run.status = RunStatus::Cancelling;
            active_run.cancellation_grace_turn_id = None;
            Ok(())
        }
        Event::CancellationGraceStarted { run_id } => {
            let active_run = active_run_mut(state, *run_id)?;
            if active_run.status != RunStatus::Cancelling {
                return Err(DomainError::InvariantViolation(
                    "only cancelling runs can enter cancellation grace".into(),
                ));
            }
            if active_run.active_turn_id.is_some() || active_run.active_tool_batch_id.is_some() {
                return Err(DomainError::InvariantViolation(
                    "cancellation grace requires drained active work".into(),
                ));
            }
            active_run.status = RunStatus::CancellingGrace;
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
            if active_run.status != RunStatus::CancellingGrace {
                return Err(DomainError::InvariantViolation(
                    "only cancellation-grace runs can become cancelled".into(),
                ));
            }
            finish_active_run(state, *run_id, RunStatus::Cancelled, None, None)
        }
        Event::ForceCancelled { run_id } => {
            let Some(active_run) = state.runs.active.as_ref() else {
                return Err(DomainError::InvariantViolation("no active run".into()));
            };
            if !matches!(
                active_run.status,
                RunStatus::Active
                    | RunStatus::Parked
                    | RunStatus::Cancelling
                    | RunStatus::CancellingGrace
            ) {
                return Err(DomainError::InvariantViolation(
                    "only non-terminal runs can be force-cancelled".into(),
                ));
            }
            finish_active_run(state, *run_id, RunStatus::Cancelled, None, None)
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
                origin: queued.origin,
                submission_digest,
                output_ref: None,
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
) -> Option<SubmissionMatch> {
    if let Some(active) = state
        .runs
        .active
        .as_ref()
        .filter(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        return Some(
            if active.origin == RunOrigin::Requested
                && active.source.matches_request(source)
                && &active.run_config == run_config
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
            if queued.origin == RunOrigin::Requested
                && queued.source.matches_request(source)
                && &queued.run_config == run_config
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
            Some(digest) if digest != request_run_submission_digest(source, run_config) => {
                SubmissionMatch::Different
            }
            _ => SubmissionMatch::Identical,
        });
    }
    if let Some(message) = state
        .runs
        .messages
        .iter()
        .find(|message| message.submission_id.as_ref() == Some(submission_id))
    {
        return Some(match message.submission_digest {
            digest if digest != request_run_submission_digest(source, run_config) => {
                SubmissionMatch::Different
            }
            _ => SubmissionMatch::Identical,
        });
    }
    None
}

pub(crate) fn match_existing_message_submission(
    state: &CoreAgentState,
    submission_id: &SubmissionId,
    input: &[ContextEntryInput],
) -> Option<SubmissionMatch> {
    if let Some(active) = state
        .runs
        .active
        .as_ref()
        .filter(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        return Some(
            if active.origin == RunOrigin::Message && active.source.matches_message_input(input) {
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
            if queued.origin == RunOrigin::Message && queued.source.matches_message_input(input) {
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
        return Some(match completed.submission_digest {
            Some(digest) if digest != message_submission_digest(input) => {
                SubmissionMatch::Different
            }
            _ => SubmissionMatch::Identical,
        });
    }
    if let Some(message) = state
        .runs
        .messages
        .iter()
        .find(|message| message.submission_id.as_ref() == Some(submission_id))
    {
        return Some(match message.submission_digest {
            digest if digest != message_submission_digest(input) => SubmissionMatch::Different,
            _ => SubmissionMatch::Identical,
        });
    }
    None
}

/// Deterministic digest of a request-run submission's payload. FNV-1a over
/// the serde_json encoding; collision resistance is not a goal — this guards
/// against client bugs, not adversaries.
pub fn request_run_submission_digest(source: &RunRequestSource, run_config: &RunConfig) -> u64 {
    submission_digest_json(&("request_run", source, run_config))
}

/// Deterministic digest of a message submission's payload. The command kind is
/// part of the digest so submission ids share one namespace across commands.
pub fn message_submission_digest(input: &[ContextEntryInput]) -> u64 {
    submission_digest_json(&("submit_message", input))
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
    match run.origin {
        RunOrigin::Requested => {
            request_run_submission_digest(&source_request_equivalent(&run.source), &run.run_config)
        }
        RunOrigin::Message => message_submission_digest(run.source.input()),
    }
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

pub(crate) fn message_mut(
    state: &mut CoreAgentState,
    message_id: MessageId,
) -> Result<&mut BufferedMessage, DomainError> {
    state
        .runs
        .messages
        .iter_mut()
        .find(|message| message.message_id == message_id)
        .ok_or_else(|| DomainError::InvariantViolation(format!("message {} not found", message_id)))
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
        let accepted = AcceptedRun {
            run_id: active_run.run_id,
            submission_id: active_run.submission_id.clone(),
            origin: active_run.origin,
            source: active_run.source.clone(),
            run_config: active_run.run_config.clone(),
            config_revision: active_run.config_revision,
            notify_on_terminal: active_run.notify_on_terminal.clone(),
        };
        submission_digest_for_accepted_run(&accepted)
    });
    state.runs.completed.push(RunRecord {
        run_id: active_run.run_id,
        status,
        submission_id: active_run.submission_id,
        origin: active_run.origin,
        submission_digest,
        output_ref,
        failure,
        notify_on_terminal: active_run.notify_on_terminal,
    });
    crate::core::components::context::expire_run_scoped_context_entries(state)?;
    Ok(())
}
