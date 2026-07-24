//! Substrate-neutral CoreAgent drive machine.
//!
//! The drive machine owns deterministic CoreAgent state and decides the next
//! action required to make progress. It does not perform async I/O, call
//! providers, invoke tools, or write storage. Local runtimes and workflow
//! substrates fulfill emitted actions and resume the drive with committed
//! entries or execution results.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const AWAIT_TOOL_NAME: &str = "await";

use crate::{
    AwaitMode, AwaitOutputRefs, AwaitSpec, BlobRef, CodecError, CommandError,
    ContextCompactionRequest, ContextCompactionResult, ContextEntryInput, ContextEntryKind,
    ContextEntrySource, ContextEvent, ContextMessageRole, CoreAgentCodec, CoreAgentEntry,
    CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus,
    DomainError, LlmFinish, LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus,
    LlmRequest, MessageStatus, PlanningError, PromiseEvent, PromiseId, PromiseStatus,
    ResumeAwaitCommand, RunEvent, RunOrigin, RunSource, SessionId, SessionPosition, ToolBatchId,
    ToolBatchOutcome, ToolCallId, ToolCallResult, ToolCallStatus, ToolEvent,
    ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationRequest,
    ToolInvocationResult, TurnEvent, TurnId, TurnOutcome, WakeReason,
    core::components::context::context_entries_from_inputs,
    session::{StoredSessionEntry, UncommittedStoredEvent},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CoreAgentAction {
    AppendEvents {
        expected_head: Option<SessionPosition>,
        events: Vec<UncommittedStoredEvent>,
    },
    GenerateLlm {
        request: LlmGenerationRequest,
    },
    CompactContext {
        request: ContextCompactionRequest,
    },
    InvokeTools {
        request: ToolInvocationBatchRequest,
    },
    Idle,
    Closed,
    StepLimitReached,
}

pub struct CoreAgentDrive {
    session_id: SessionId,
    state: CoreAgentState,
    head: Option<SessionPosition>,
    steps_taken: usize,
}

impl CoreAgentDrive {
    pub fn from_replayed(
        session_id: SessionId,
        state: CoreAgentState,
        head: Option<SessionPosition>,
    ) -> Self {
        debug_assert_eq!(state.reduced_to, head);
        Self {
            session_id,
            state,
            head,
            steps_taken: 0,
        }
    }

    pub fn admit_command(
        &mut self,
        command: crate::CoreAgentCommand,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = crate::core::admit::admit_command(&self.state, command, observed_at_ms)?;
        self.append_action(proposals, observed_at_ms)
    }

    pub fn next_action(
        &mut self,
        observed_at_ms: u64,
        max_steps: usize,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = crate::core::planning::plan_next(&self.state)?;
        if !proposals.is_empty() {
            if !self.increment_steps(max_steps) {
                return Ok(CoreAgentAction::StepLimitReached);
            }
            return self.append_action(proposals, observed_at_ms);
        }

        if let Some(request) = next_generation_request(&self.session_id, &self.state)? {
            if !self.increment_steps(max_steps) {
                return Ok(CoreAgentAction::StepLimitReached);
            }
            return Ok(CoreAgentAction::GenerateLlm { request });
        }

        if let Some(request) = next_context_compaction_request(&self.session_id, &self.state)? {
            if !self.increment_steps(max_steps) {
                return Ok(CoreAgentAction::StepLimitReached);
            }
            return Ok(CoreAgentAction::CompactContext { request });
        }

        if let Some(request) = next_tool_batch_request(&self.session_id, &self.state)? {
            if !self.increment_steps(max_steps) {
                return Ok(CoreAgentAction::StepLimitReached);
            }
            return Ok(CoreAgentAction::InvokeTools { request });
        }

        Ok(classify_core_agent_action(&self.state))
    }

    pub fn resume_appended(
        &mut self,
        entries: Vec<StoredSessionEntry>,
    ) -> Result<Vec<CoreAgentEntry>, CoreAgentDriveError> {
        let decoded = entries
            .iter()
            .map(|entry| CoreAgentCodec.decode_entry(entry))
            .collect::<Result<Vec<_>, _>>()?;
        for entry in &decoded {
            crate::core::apply::apply_event(&mut self.state, entry)?;
        }
        self.head = self.state.reduced_to.clone();
        Ok(decoded)
    }

    pub fn resume_generation(
        &mut self,
        result: LlmGenerationResult,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = generation_result_proposals(&self.state, result)?;
        self.append_action(proposals, observed_at_ms)
    }

    pub fn resume_context_compaction(
        &mut self,
        result: ContextCompactionResult,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        if result.session_id != self.session_id {
            return Err(DomainError::InvariantViolation(format!(
                "context compaction result session {} does not match drive session {}",
                result.session_id, self.session_id
            ))
            .into());
        }
        let proposals = context_compaction_result_proposals(&self.state, result)?;
        self.append_action(proposals, observed_at_ms)
    }

    pub fn resume_tool_batch(
        &mut self,
        result: ToolInvocationBatchResult,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals =
            tool_batch_result_proposals_for_session(&self.session_id, &self.state, result)?;
        self.append_action(proposals, observed_at_ms)
    }

    pub fn resume_tool_batch_outcome(
        &mut self,
        outcome: ToolBatchOutcome,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        match outcome {
            ToolBatchOutcome::Completed { result } => {
                self.resume_tool_batch(result, observed_at_ms)
            }
            ToolBatchOutcome::Deferred {
                batch_id,
                call_id,
                completed_results,
                spec,
            } => self.defer_tool_batch(batch_id, call_id, completed_results, spec, observed_at_ms),
        }
    }

    pub fn defer_tool_batch(
        &mut self,
        batch_id: ToolBatchId,
        call_id: ToolCallId,
        completed_results: Vec<ToolInvocationResult>,
        spec: AwaitSpec,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = tool_batch_deferred_proposals(
            &self.session_id,
            &self.state,
            batch_id,
            call_id,
            completed_results,
            spec,
        )?;
        self.append_action(proposals, observed_at_ms)
    }

    pub fn reset_steps(&mut self) {
        self.steps_taken = 0;
    }

    pub fn state(&self) -> &CoreAgentState {
        &self.state
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn head(&self) -> Option<&SessionPosition> {
        self.head.as_ref()
    }

    fn append_action(
        &self,
        proposals: Vec<CoreAgentEventProposal>,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        if proposals.is_empty() {
            return Ok(classify_core_agent_action(&self.state));
        }
        let proposals = with_run_terminal_side_effects(&self.state, proposals);
        let events = proposals
            .into_iter()
            .map(|proposal| proposal.into_uncommitted(observed_at_ms))
            .map(|event| CoreAgentCodec.encode_uncommitted(&event))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CoreAgentAction::AppendEvents {
            expected_head: self.head.clone(),
            events,
        })
    }

    fn increment_steps(&mut self, max_steps: usize) -> bool {
        if self.steps_taken >= max_steps {
            return false;
        }
        self.steps_taken += 1;
        true
    }
}

fn with_run_terminal_side_effects(
    state: &CoreAgentState,
    proposals: Vec<CoreAgentEventProposal>,
) -> Vec<CoreAgentEventProposal> {
    let mut output = Vec::with_capacity(proposals.len());
    let mut cancelled = BTreeSet::<PromiseId>::new();
    let mut next_run_id = state.id_cursors.last_run_id;
    let cancels_buffered_messages = proposals.iter().any(|proposal| {
        matches!(
            proposal.event,
            CoreAgentEvent::Run(RunEvent::MessageCancelled { .. })
        )
    });
    for proposal in proposals {
        let terminal_run_id = terminal_run_id_for_proposal(&proposal);
        output.push(proposal.clone());
        let Some(run_id) = terminal_run_id else {
            continue;
        };
        for promise in state.promises.pending_for_run(run_id) {
            if promise.status != PromiseStatus::Pending
                || !cancelled.insert(promise.promise_id.clone())
            {
                continue;
            }
            output.push(CoreAgentEventProposal::new(
                CoreAgentJoins {
                    run_id: Some(run_id),
                    ..CoreAgentJoins::default()
                },
                CoreAgentEvent::Promise(PromiseEvent::Cancelled {
                    promise_id: promise.promise_id.clone(),
                }),
            ));
        }
        if cancels_buffered_messages {
            continue;
        }
        for message in state
            .runs
            .messages
            .iter()
            .filter(|message| message.status == MessageStatus::Buffered)
        {
            let Some(next) = next_run_id.checked_add(1) else {
                continue;
            };
            let promoted_run_id = crate::RunId::new(next);
            next_run_id = next;
            let joins = CoreAgentJoins {
                run_id: Some(promoted_run_id),
                submission_id: message.submission_id.clone(),
                ..CoreAgentJoins::default()
            };
            output.push(CoreAgentEventProposal::new(
                joins.clone(),
                CoreAgentEvent::Run(RunEvent::Accepted(crate::AcceptedRunEvent {
                    run_id: promoted_run_id,
                    submission_id: message.submission_id.clone(),
                    origin: RunOrigin::Message,
                    source: RunSource::Input {
                        input: message.input.clone(),
                    },
                    run_config: message.run_config.clone(),
                    config_revision: message.config_revision,
                    notify_on_terminal: Vec::new(),
                })),
            ));
            output.push(CoreAgentEventProposal::new(
                joins,
                CoreAgentEvent::Run(RunEvent::MessagePromotedToRun {
                    message_id: message.message_id,
                    run_id: promoted_run_id,
                }),
            ));
        }
    }
    output
}

fn terminal_run_id_for_proposal(proposal: &CoreAgentEventProposal) -> Option<crate::RunId> {
    match &proposal.event {
        CoreAgentEvent::Run(
            RunEvent::Completed { run_id, .. }
            | RunEvent::Failed { run_id, .. }
            | RunEvent::Cancelled { run_id }
            | RunEvent::ForceCancelled { run_id },
        ) => Some(*run_id),
        CoreAgentEvent::Run(RunEvent::QueuedCancelled { .. }) => None,
        _ => None,
    }
}

#[derive(Debug, Error)]
pub enum CoreAgentDriveError {
    #[error(transparent)]
    Command(#[from] CommandError),

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Planning(#[from] PlanningError),
}

pub fn classify_core_agent_action(state: &CoreAgentState) -> CoreAgentAction {
    if state.lifecycle.status == CoreAgentStatus::Closed {
        CoreAgentAction::Closed
    } else {
        CoreAgentAction::Idle
    }
}

pub fn next_generation_request(
    session_id: &SessionId,
    state: &CoreAgentState,
) -> Result<Option<LlmGenerationRequest>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(None);
    };
    let Some(turn_id) = active_run.active_turn_id else {
        return Ok(None);
    };
    let turn = active_run.turns.get(&turn_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active turn {} is missing", turn_id))
    })?;
    if turn.status != crate::TurnStatus::GenerationPending {
        return Ok(None);
    }
    let planned = turn.planned_request.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation(
            "generation-pending turn is missing planned request metadata".into(),
        )
    })?;
    let request = crate::core::components::llm::build_planned_llm_request(
        state, active_run, turn_id, planned,
    )?;
    Ok(Some(LlmGenerationRequest {
        session_id: session_id.clone(),
        run_id: active_run.run_id,
        turn_id,
        request,
    }))
}

pub fn rebuild_llm_request_for_planned_turn(
    entries: &[CoreAgentEntry],
    target_turn_id: TurnId,
) -> Result<Option<LlmRequest>, DomainError> {
    let mut state = CoreAgentState::new();
    for entry in entries {
        if let CoreAgentEvent::Turn(TurnEvent::Planned {
            turn_id,
            run_id,
            request_fingerprint,
            config_revision,
            context_revision,
            toolset_revision,
        }) = &entry.event
            && *turn_id == target_turn_id
        {
            let active_run = state.runs.active.as_ref().ok_or_else(|| {
                DomainError::InvariantViolation(
                    "planned turn reconstruction requires an active run".into(),
                )
            })?;
            if active_run.run_id != *run_id || active_run.active_turn_id != Some(*turn_id) {
                return Err(DomainError::InvariantViolation(
                    "planned turn reconstruction run/turn does not match active state".into(),
                ));
            }
            let planned = crate::PlannedRequestState {
                request_fingerprint: request_fingerprint.clone(),
                config_revision: *config_revision,
                context_revision: *context_revision,
                toolset_revision: *toolset_revision,
            };
            return crate::core::components::llm::build_planned_llm_request(
                &state, active_run, *turn_id, &planned,
            )
            .map(Some);
        }
        crate::core::apply::apply_event(&mut state, entry)?;
    }
    Ok(None)
}

pub fn next_context_compaction_request(
    session_id: &SessionId,
    state: &CoreAgentState,
) -> Result<Option<ContextCompactionRequest>, DomainError> {
    if !state.context.pending_compaction {
        return Ok(None);
    }
    let request = crate::core::components::llm::build_context_compaction_task(state)
        .map_err(|error| DomainError::InvariantViolation(error.to_string()))?;
    Ok(Some(ContextCompactionRequest {
        session_id: session_id.clone(),
        request,
    }))
}

pub fn generation_result_proposals(
    state: &CoreAgentState,
    result: LlmGenerationResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let active_run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
    if active_run.run_id != result.run_id || active_run.active_turn_id != Some(result.turn_id) {
        return Err(DomainError::InvariantViolation(
            "llm generation result does not match active turn".into(),
        ));
    }
    let context_entries = context_entries_from_llm_result(state, &result)?;
    let outcome = turn_outcome_for_generation_result(&result);
    let joins = CoreAgentJoins {
        run_id: Some(result.run_id),
        turn_id: Some(result.turn_id),
        ..CoreAgentJoins::default()
    };

    let mut proposals = Vec::new();
    if !context_entries.is_empty() {
        proposals.push(CoreAgentEventProposal::new(
            joins.clone(),
            CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision: state.context.revision,
                entries: context_entries,
            }),
        ));
    }
    proposals.push(CoreAgentEventProposal::new(
        joins.clone(),
        CoreAgentEvent::Turn(TurnEvent::GenerationCompleted {
            turn_id: result.turn_id,
            run_id: result.run_id,
            status: result.status,
            facts: result.facts,
        }),
    ));
    proposals.push(CoreAgentEventProposal::new(
        joins,
        CoreAgentEvent::Turn(TurnEvent::Completed {
            turn_id: result.turn_id,
            outcome,
        }),
    ));
    Ok(proposals)
}

fn turn_outcome_for_generation_result(result: &LlmGenerationResult) -> TurnOutcome {
    match &result.status {
        LlmGenerationStatus::Cancelled => TurnOutcome::Cancelled,
        LlmGenerationStatus::Failed => TurnOutcome::Failed {
            failure_ref: result.failure_ref.clone(),
        },
        LlmGenerationStatus::Succeeded => match result.facts.finish {
            LlmFinish::ToolCalls => TurnOutcome::ToolCallsQueued,
            LlmFinish::ContextLimit => TurnOutcome::ContextUpdateRequired,
            LlmFinish::Cancelled => TurnOutcome::Cancelled,
            LlmFinish::Failed => TurnOutcome::Failed {
                failure_ref: result.failure_ref.clone(),
            },
            LlmFinish::Stop | LlmFinish::Length | LlmFinish::ContentFilter | LlmFinish::Unknown => {
                TurnOutcome::FinalOutput {
                    output_ref: final_output_ref(&result.context_entries),
                }
            }
        },
    }
}

fn context_entries_from_llm_result(
    state: &CoreAgentState,
    result: &LlmGenerationResult,
) -> Result<Vec<crate::ContextEntry>, DomainError> {
    context_entries_from_inputs(
        state,
        result
            .context_entries
            .iter()
            .cloned()
            .map(|entry| {
                (
                    None,
                    source_for_llm_context_entry(result.run_id, result.turn_id, &entry),
                    entry,
                )
            })
            .collect(),
    )
}

fn source_for_llm_context_entry(
    run_id: crate::RunId,
    turn_id: crate::TurnId,
    entry: &ContextEntryInput,
) -> ContextEntrySource {
    match &entry.kind {
        ContextEntryKind::ReasoningState => ContextEntrySource::Reasoning { run_id, turn_id },
        _ => ContextEntrySource::AssistantOutput { run_id, turn_id },
    }
}

fn final_output_ref(context_entries: &[ContextEntryInput]) -> Option<BlobRef> {
    context_entries
        .iter()
        .rev()
        .find_map(|entry| match entry.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(entry.content_ref.clone()),
            _ => None,
        })
        .or_else(|| {
            context_entries
                .last()
                .map(|entry| entry.content_ref.clone())
        })
}

pub fn context_compaction_result_proposals(
    state: &CoreAgentState,
    result: ContextCompactionResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    if !state.context.pending_compaction {
        return Err(DomainError::InvariantViolation(
            "context compaction result received without pending request".to_owned(),
        ));
    }
    if result.context_revision != state.context.revision {
        return Err(DomainError::InvariantViolation(format!(
            "context compaction result revision {} does not match active context revision {}",
            result.context_revision, state.context.revision
        )));
    }
    let mut proposals = Vec::new();
    let mut base_revision = state.context.revision;
    if !result.context_entries.is_empty() {
        let entries = context_entries_from_inputs(
            state,
            result
                .context_entries
                .iter()
                .cloned()
                .map(|entry| {
                    (
                        None,
                        ContextEntrySource::Runtime {
                            label: "provider_standalone_compaction".to_owned(),
                        },
                        entry,
                    )
                })
                .collect(),
        )?;
        proposals.push(CoreAgentEventProposal::new(
            CoreAgentJoins::default(),
            CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision,
                entries,
            }),
        ));
        base_revision = base_revision.checked_add(1).ok_or_else(|| {
            DomainError::InvariantViolation("context revision exhausted".to_owned())
        })?;
    }
    proposals.push(CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEvent::Context(ContextEvent::CompactionFinished {
            base_revision,
            status: result.status,
            failure_ref: result.failure_ref,
        }),
    ));
    Ok(proposals)
}

pub fn next_tool_batch_request(
    session_id: &SessionId,
    state: &CoreAgentState,
) -> Result<Option<ToolInvocationBatchRequest>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(None);
    };
    let Some(batch_id) = active_run.active_tool_batch_id else {
        return Ok(None);
    };
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active tool batch {} is missing", batch_id))
    })?;
    if active_run
        .parked_await
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Ok(None);
    }
    let calls = batch
        .calls
        .iter()
        .filter(|call_state| call_state.status == ToolCallStatus::Pending)
        .map(|call_state| ToolInvocationRequest {
            call_id: call_state.call.call_id.clone(),
            tool_name: call_state.call.tool_name.clone(),
            arguments_ref: call_state.call.arguments_ref.clone(),
            execution_target: call_state.execution_target.clone(),
        })
        .collect::<Vec<_>>();
    if calls.is_empty() {
        return Ok(None);
    }
    Ok(Some(ToolInvocationBatchRequest {
        session_id: session_id.clone(),
        run_id: batch.run_id,
        turn_id: batch.turn_id,
        batch_id: batch.batch_id,
        default_targets: state.tooling.routing.default_targets.clone(),
        calls,
    }))
}

pub fn tool_batch_deferred_proposals(
    session_id: &SessionId,
    state: &CoreAgentState,
    batch_id: ToolBatchId,
    call_id: ToolCallId,
    completed_results: Vec<ToolInvocationResult>,
    spec: AwaitSpec,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let active_run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
    if active_run.active_tool_batch_id != Some(batch_id) {
        return Err(DomainError::InvariantViolation(
            "deferred tool batch does not match active tool batch".into(),
        ));
    }
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    if active_run.parked_await.is_some() {
        return Err(DomainError::InvariantViolation(format!(
            "tool batch {} is already deferred",
            batch_id
        )));
    }
    let completed_result = ToolInvocationBatchResult {
        run_id: batch.run_id,
        turn_id: batch.turn_id,
        batch_id: batch.batch_id,
        results: completed_results,
    };
    validate_tool_batch_result(&completed_result)?;
    validate_result_matches_active_tool_batch(state, &completed_result, false)?;
    let completed_call_ids = completed_result
        .results
        .iter()
        .map(|result| result.call_id.clone())
        .collect::<BTreeSet<_>>();
    if !batch.calls.iter().any(|call_state| {
        call_state.status == ToolCallStatus::Pending
            && !completed_call_ids.contains(&call_state.call.call_id)
    }) {
        return Err(DomainError::InvariantViolation(
            "tool batch deferral requires at least one pending call".into(),
        ));
    }
    if batch.calls.iter().any(|call_state| {
        matches!(
            call_state.status,
            ToolCallStatus::Observed | ToolCallStatus::Accepted
        )
    }) {
        return Err(DomainError::InvariantViolation(
            "tool batch deferral requires all invocable calls to be pending".into(),
        ));
    }
    let pending_await_call_ids = batch
        .calls
        .iter()
        .filter(|call_state| {
            call_state.status == ToolCallStatus::Pending
                && call_state.call.tool_name.as_str() == AWAIT_TOOL_NAME
        })
        .map(|call_state| call_state.call.call_id.clone())
        .collect::<Vec<_>>();
    if pending_await_call_ids.len() > 1 {
        let mut results = completed_result.results;
        for call_id in pending_await_call_ids {
            results.push(invalid_await_tool_result(
                call_id,
                "only one await call is allowed per tool batch".to_owned(),
            ));
        }
        return tool_batch_result_proposals(
            state,
            ToolInvocationBatchResult {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
                batch_id: batch.batch_id,
                results,
            },
        );
    }
    let await_call_is_pending = batch.calls.iter().any(|call_state| {
        call_state.call.call_id == call_id && call_state.status == ToolCallStatus::Pending
    });
    if !await_call_is_pending {
        return Err(DomainError::InvariantViolation(format!(
            "await deferral references non-pending call {}",
            call_id
        )));
    }
    if !batch.calls.iter().any(|call_state| {
        call_state.call.call_id == call_id && call_state.call.tool_name.as_str() == AWAIT_TOOL_NAME
    }) {
        return Err(DomainError::InvariantViolation(format!(
            "deferred call {} is not an await call",
            call_id
        )));
    }
    if let Err(error) = validate_await_spec_for_active_run(state, active_run.run_id, &spec) {
        let mut results = completed_result.results;
        results.push(invalid_await_tool_result(call_id, error.to_string()));
        return tool_batch_result_proposals(
            state,
            ToolInvocationBatchResult {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
                batch_id: batch.batch_id,
                results,
            },
        );
    }
    let joins = CoreAgentJoins {
        run_id: Some(batch.run_id),
        turn_id: Some(batch.turn_id),
        tool_batch_id: Some(batch.batch_id),
        ..CoreAgentJoins::default()
    };
    let mut proposals = tool_call_completed_proposals(state, Some(session_id), completed_result)?;
    proposals.push(CoreAgentEventProposal::new(
        joins,
        CoreAgentEvent::Tool(ToolEvent::BatchDeferred {
            run_id: batch.run_id,
            turn_id: batch.turn_id,
            batch_id: batch.batch_id,
            call_id,
            spec,
        }),
    ));
    Ok(proposals)
}

pub fn tool_batch_result_proposals(
    state: &CoreAgentState,
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    tool_batch_result_proposals_inner(None, state, result)
}

fn tool_batch_result_proposals_for_session(
    session_id: &SessionId,
    state: &CoreAgentState,
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    tool_batch_result_proposals_inner(Some(session_id), state, result)
}

fn tool_batch_result_proposals_inner(
    session_id: Option<&SessionId>,
    state: &CoreAgentState,
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    validate_tool_batch_result(&result)?;
    validate_result_matches_active_tool_batch(state, &result, false)?;
    tool_call_completed_proposals(state, session_id, result)
}

pub fn resume_await_proposals(
    state: &CoreAgentState,
    command: ResumeAwaitCommand,
    observed_at_ms: u64,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    if active_run.run_id != command.run_id {
        return Ok(Vec::new());
    }
    let Some(parked) = active_run.parked_await.as_ref() else {
        return Ok(Vec::new());
    };
    if parked.batch_id != command.batch_id {
        return Ok(Vec::new());
    }
    if command.claim_observed_at_ms > observed_at_ms {
        return Err(DomainError::InvariantViolation(
            "resume await claim is observed in the future".to_owned(),
        ));
    }
    let Some(actual) = await_wake(state, command.claim_observed_at_ms) else {
        return Err(DomainError::InvariantViolation(
            "resume await claim has no satisfied wake".to_owned(),
        ));
    };
    if actual != command.claim {
        return Err(DomainError::InvariantViolation(format!(
            "resume await claim {:?} does not match current wake {:?}",
            command.claim, actual
        )));
    }
    let result = await_resume_result(state, command.output)?;
    validate_tool_batch_result(&result)?;
    validate_result_matches_active_tool_batch(state, &result, true)?;
    let joins = CoreAgentJoins {
        run_id: Some(result.run_id),
        turn_id: Some(result.turn_id),
        tool_batch_id: Some(result.batch_id),
        ..CoreAgentJoins::default()
    };
    let mut proposals = vec![CoreAgentEventProposal::new(
        joins.clone(),
        CoreAgentEvent::Tool(ToolEvent::BatchResumed {
            run_id: result.run_id,
            turn_id: result.turn_id,
            batch_id: result.batch_id,
        }),
    )];
    if command.claim == WakeReason::MailboxMessage {
        for message in buffered_mailbox_messages(state) {
            proposals.push(CoreAgentEventProposal::new(
                joins.clone(),
                CoreAgentEvent::Run(RunEvent::MessageConsumedByAwait {
                    message_id: message.message_id,
                    run_id: result.run_id,
                }),
            ));
        }
    }
    proposals.extend(tool_call_completed_proposals(state, None, result)?);
    Ok(proposals)
}

pub fn await_wake(state: &CoreAgentState, now_ms: u64) -> Option<WakeReason> {
    let active_run = state.runs.active.as_ref()?;
    let parked = active_run.parked_await.as_ref()?;
    if matches!(
        active_run.status,
        crate::RunStatus::Cancelling | crate::RunStatus::CancellingGrace
    ) {
        return Some(WakeReason::Cancelled);
    }
    if parked.spec.mailbox && !buffered_mailbox_messages(state).is_empty() {
        return Some(WakeReason::MailboxMessage);
    }
    if parked
        .spec
        .deadline_at_ms
        .is_some_and(|deadline| deadline <= now_ms)
    {
        return Some(WakeReason::Timeout);
    }
    if parked.spec.promise_ids.is_empty() {
        return None;
    }
    let terminal = parked
        .spec
        .promise_ids
        .iter()
        .filter_map(|promise_id| state.promises.promises.get(promise_id))
        .filter(|promise| promise.status.is_terminal())
        .count();
    match parked.spec.mode {
        AwaitMode::All if terminal == parked.spec.promise_ids.len() => Some(WakeReason::Terminal),
        AwaitMode::Any if terminal >= 1 => Some(WakeReason::Terminal),
        _ => None,
    }
}

fn validate_await_spec_for_active_run(
    state: &CoreAgentState,
    run_id: crate::RunId,
    spec: &AwaitSpec,
) -> Result<(), DomainError> {
    if spec.promise_ids.is_empty() && !spec.mailbox {
        return Err(DomainError::InvariantViolation(
            "await requires at least one promise id or mailbox=true".to_owned(),
        ));
    }
    for promise_id in &spec.promise_ids {
        let Some(promise) = state.promises.promises.get(promise_id) else {
            return Err(DomainError::InvariantViolation(format!(
                "unknown promise {}",
                promise_id
            )));
        };
        match promise.scope {
            crate::PromiseScope::Run {
                run_id: promise_run_id,
            } if promise_run_id != run_id => {
                return Err(DomainError::InvariantViolation(format!(
                    "promise {} is scoped to run {}, not run {}",
                    promise_id, promise_run_id, run_id
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn await_resume_result(
    state: &CoreAgentState,
    output: AwaitOutputRefs,
) -> Result<ToolInvocationBatchResult, DomainError> {
    let active_run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
    let parked = active_run.parked_await.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation("resume await requires a parked await".to_owned())
    })?;
    let batch = active_run
        .tool_batches
        .get(&parked.batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {} is missing", parked.batch_id))
        })?;
    let mut model_visible_context_entries = vec![ToolInvocationResult::tool_result_context_entry(
        &parked.call_id,
        ToolCallStatus::Succeeded,
        output.summary_ref,
    )];
    for promise_id in &parked.spec.promise_ids {
        let Some(promise) = state.promises.promises.get(promise_id) else {
            continue;
        };
        if let Some(payload_ref) = promise.payload_ref.clone() {
            model_visible_context_entries.push(await_user_message(
                payload_ref,
                Some(format!("Promise {} resolved output", promise_id)),
            ));
        } else if let Some(error_ref) = promise.error_ref.clone() {
            model_visible_context_entries.push(await_user_message(
                error_ref,
                Some(format!("Promise {} failure detail", promise_id)),
            ));
        }
    }
    for message in buffered_mailbox_messages(state) {
        model_visible_context_entries.extend(message.input.iter().cloned());
    }
    Ok(ToolInvocationBatchResult {
        run_id: active_run.run_id,
        turn_id: batch.turn_id,
        batch_id: batch.batch_id,
        results: vec![ToolInvocationResult {
            call_id: parked.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output.output_ref),
            model_visible_context_entries,
            error_ref: None,
            effects: Vec::new(),
        }],
    })
}

fn await_user_message(content_ref: BlobRef, preview: Option<String>) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: None,
        preview: preview.map(|value| value.chars().take(160).collect()),
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

fn buffered_mailbox_messages(state: &CoreAgentState) -> Vec<&crate::BufferedMessage> {
    state
        .runs
        .messages
        .iter()
        .filter(|message| message.status == MessageStatus::Buffered)
        .collect()
}

fn invalid_await_tool_result(call_id: ToolCallId, _message: String) -> ToolInvocationResult {
    let error_ref = crate::unavailable_tool_result_ref();
    ToolInvocationResult {
        call_id: call_id.clone(),
        status: ToolCallStatus::Failed,
        output_ref: None,
        model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
            &call_id,
            ToolCallStatus::Failed,
            error_ref.clone(),
        )],
        error_ref: Some(error_ref),
        effects: Vec::new(),
    }
}

fn tool_call_completed_proposals(
    state: &CoreAgentState,
    session_id: Option<&SessionId>,
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let mut proposals = Vec::new();
    let mut resolved_promises = BTreeSet::new();
    let mut pending_port_emissions = BTreeMap::<crate::WorkflowToolPortId, u32>::new();
    for result_item in result.results {
        let call_id = result_item.call_id.clone();
        let joins = CoreAgentJoins {
            run_id: Some(result.run_id),
            turn_id: Some(result.turn_id),
            tool_batch_id: Some(result.batch_id),
            tool_call_id: Some(call_id.clone()),
            ..CoreAgentJoins::default()
        };
        // Promise creations ride tool effects: each becomes an explicit
        // log event in the same append as the call completion, so promise
        // state is rebuilt from the log like everything else.
        let mut promise_proposals = Vec::new();
        let mut port_proposals = Vec::new();
        let mut saw_port_effect = false;
        for effect in &result_item.effects {
            if let Some(promise) =
                crate::core::components::promise::promise_from_create_effect(effect, result.run_id)?
            {
                promise_proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::Promise(PromiseEvent::Created { promise }),
                ));
            }
            if let Some(promise_id) =
                crate::core::components::promise::promise_id_from_cancel_effect(effect)?
            {
                let Some(promise) = state.promises.promises.get(&promise_id) else {
                    return Err(DomainError::InvariantViolation(format!(
                        "promise cancel effect references unknown promise {}",
                        promise_id
                    )));
                };
                if promise.status.is_terminal() || !resolved_promises.insert(promise_id.clone()) {
                    continue;
                }
                promise_proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::Promise(PromiseEvent::Cancelled { promise_id }),
                ));
            }
            if let Some(promise_id) =
                crate::core::components::promise::promise_id_from_detach_effect(effect)?
            {
                let Some(promise) = state.promises.promises.get(&promise_id) else {
                    return Err(DomainError::InvariantViolation(format!(
                        "promise detach effect references unknown promise {}",
                        promise_id
                    )));
                };
                if promise.status.is_terminal() {
                    continue;
                }
                match promise.scope {
                    crate::PromiseScope::Session => continue,
                    crate::PromiseScope::Run { run_id } if run_id == result.run_id => {}
                    crate::PromiseScope::Run { run_id } => {
                        return Err(DomainError::InvariantViolation(format!(
                            "promise detach effect references promise {} scoped to run {}, not result run {}",
                            promise_id, run_id, result.run_id
                        )));
                    }
                }
                promise_proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::Promise(PromiseEvent::Detached { promise_id }),
                ));
            }
            if let Some(invocation) =
                crate::core::components::workflow_port::invocation_from_emit_effect(effect)?
            {
                if saw_port_effect {
                    return Err(DomainError::InvariantViolation(format!(
                        "tool call {} produced more than one workflow port emission effect",
                        call_id
                    )));
                }
                saw_port_effect = true;
                if result_item.status != ToolCallStatus::Succeeded {
                    return Err(DomainError::InvariantViolation(format!(
                        "failed tool call {} produced a workflow port emission effect",
                        call_id
                    )));
                }
                let session_id = session_id.ok_or_else(|| {
                    DomainError::InvariantViolation(
                        "workflow port emission effect was admitted without session identity"
                            .to_owned(),
                    )
                })?;
                let pending = pending_port_emissions
                    .get(&invocation.port_id)
                    .copied()
                    .unwrap_or(0);
                crate::core::components::workflow_port::validate_emit_effect(
                    state,
                    session_id,
                    result.run_id,
                    result.turn_id,
                    result.batch_id,
                    &call_id,
                    &invocation,
                    pending,
                )?;
                pending_port_emissions
                    .insert(invocation.port_id.clone(), pending.saturating_add(1));
                port_proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::WorkflowPort(crate::WorkflowPortEvent::Emitted { invocation }),
                ));
            }
        }
        proposals.push(CoreAgentEventProposal::new(
            joins,
            CoreAgentEvent::Tool(ToolEvent::CallCompleted {
                run_id: result.run_id,
                turn_id: result.turn_id,
                batch_id: result.batch_id,
                result: invocation_result_to_call_result(result_item),
            }),
        ));
        proposals.extend(promise_proposals);
        proposals.extend(port_proposals);
    }
    Ok(proposals)
}

fn validate_tool_batch_result(result: &ToolInvocationBatchResult) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for result in &result.results {
        if !seen.insert(result.call_id.clone()) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate tool invocation result for call {}",
                result.call_id
            )));
        }
        if !matches!(
            result.status,
            ToolCallStatus::Succeeded | ToolCallStatus::Failed | ToolCallStatus::Cancelled
        ) {
            return Err(DomainError::InvariantViolation(
                "tool invocation result must have a terminal call status".into(),
            ));
        }
    }
    Ok(())
}

fn validate_result_matches_active_tool_batch(
    state: &CoreAgentState,
    result: &ToolInvocationBatchResult,
    require_parked: bool,
) -> Result<(), DomainError> {
    let active_run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
    if active_run.run_id != result.run_id
        || active_run.active_tool_batch_id != Some(result.batch_id)
    {
        return Err(DomainError::InvariantViolation(
            "tool invocation result does not match active tool batch".into(),
        ));
    }
    let batch = active_run
        .tool_batches
        .get(&result.batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {} is missing", result.batch_id))
        })?;
    if batch.turn_id != result.turn_id {
        return Err(DomainError::InvariantViolation(
            "tool invocation result does not match active turn".into(),
        ));
    }
    let is_parked = active_run
        .parked_await
        .as_ref()
        .is_some_and(|parked| parked.batch_id == result.batch_id);
    match (require_parked, is_parked) {
        (true, false) => {
            return Err(DomainError::InvariantViolation(format!(
                "tool batch {} is not deferred",
                result.batch_id
            )));
        }
        (false, true) => {
            return Err(DomainError::InvariantViolation(
                "deferred tool batch must be resumed by command".into(),
            ));
        }
        _ => {}
    }
    for result_item in &result.results {
        let call_state = batch
            .calls
            .iter()
            .find(|call_state| call_state.call.call_id == result_item.call_id)
            .ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "tool invocation result references missing call {}",
                    result_item.call_id
                ))
            })?;
        if call_state.status != ToolCallStatus::Pending {
            return Err(DomainError::InvariantViolation(
                "tool invocation result requires pending tool calls".into(),
            ));
        }
    }
    Ok(())
}

fn invocation_result_to_call_result(result: ToolInvocationResult) -> ToolCallResult {
    ToolCallResult {
        call_id: result.call_id,
        status: result.status,
        output_ref: result.output_ref,
        model_visible_context_entries: result.model_visible_context_entries,
        error_ref: result.error_ref,
        effects: result.effects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlobRef, CommandRejectionDetails, CommandRejectionKind, CompactionPolicy,
        ContextCompactionStatus, ContextCompactionTrigger, ContextConfig, ContextEntry,
        ContextEntryId, ContextEntryInput, ContextEntryKey, ContextEntryKind, ContextRemovalReason,
        ContextRewriteReason, CoreAgentCommand, FunctionToolSpec, LlmGenerationFacts,
        ModelSelection, OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND, ObservedToolCall,
        ProviderApiKind, RunConfig, RunFailureKind, RunRequestCommand, RunRequestSource, RunStatus,
        SKILL_ACTIVATION_PROVIDER_KIND_RUN, SKILL_CATALOG_CONTEXT_KEY, SessionConfig, SkillId,
        SubmitMessageCommand, TokenEstimate, TokenEstimateQuality, ToolBatchOutcome, ToolChoice,
        ToolEffect, ToolInvocationResult, ToolKind, ToolName, ToolParallelism, ToolSpec,
        ToolTargetRequirement, TurnStatus, WorkflowEndpointRef, WorkflowToolInvocation,
        WorkflowToolPortDefinition, WorkflowToolPortId, skill_activation_context_key,
    };

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            generation: Default::default(),
            limits: Default::default(),
            context: ContextConfig { compaction: None },
            features: Default::default(),
        }
    }

    fn run_config() -> RunConfig {
        RunConfig::default()
    }

    fn standalone_compaction_config(
        compact_threshold_tokens: Option<u32>,
        target_tokens: Option<u32>,
    ) -> SessionConfig {
        let mut config = config();
        config.context.compaction = Some(CompactionPolicy::ProviderStandalone {
            compact_threshold_tokens,
            target_tokens,
        });
        config
    }

    fn commit_action(drive: &mut CoreAgentDrive, action: CoreAgentAction) -> Vec<CoreAgentEntry> {
        let CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } = action
        else {
            panic!("expected append action");
        };
        assert_eq!(expected_head, drive.head().cloned());
        let mut head = expected_head;
        let entries = events
            .into_iter()
            .map(|event| {
                let seq = head
                    .as_ref()
                    .map_or(1, |position| position.seq.as_u64() + 1);
                let position = SessionPosition {
                    seq: crate::EventSeq::new(seq),
                };
                head = Some(position.clone());
                StoredSessionEntry {
                    position,
                    observed_at_ms: event.observed_at_ms,
                    joins: event.joins,
                    event: event.event,
                }
            })
            .collect::<Vec<_>>();
        drive.resume_appended(entries).expect("resume appended")
    }

    fn commit_core_event_result(
        drive: &mut CoreAgentDrive,
        kind: CoreAgentEvent,
        observed_at_ms: u64,
    ) -> Result<Vec<CoreAgentEntry>, CoreAgentDriveError> {
        let proposal = CoreAgentEventProposal::new(CoreAgentJoins::default(), kind);
        let uncommitted = proposal.into_uncommitted(observed_at_ms);
        let event = CoreAgentCodec.encode_uncommitted(&uncommitted)?;
        let seq = drive.head().map_or(1, |position| position.seq.as_u64() + 1);
        let entry = StoredSessionEntry {
            position: SessionPosition {
                seq: crate::EventSeq::new(seq),
            },
            observed_at_ms: event.observed_at_ms,
            joins: event.joins,
            event: event.event,
        };
        drive.resume_appended(vec![entry])
    }

    fn context_edit_entry(
        entry_id: u64,
        key: Option<ContextEntryKey>,
        content: &'static [u8],
    ) -> ContextEntry {
        ContextEntry {
            entry_id: ContextEntryId::new(entry_id),
            key,
            kind: ContextEntryKind::ProviderOpaque,
            source: ContextEntrySource::ContextEdit,
            content_ref: BlobRef::from_bytes(content),
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn open_session(drive: &mut CoreAgentDrive) {
        open_session_with_config(drive, config());
    }

    fn open_session_with_config(drive: &mut CoreAgentDrive, config: SessionConfig) {
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config }, 10)
            .expect("open");
        commit_action(drive, open);
    }

    fn request_run(drive: &mut CoreAgentDrive, input_ref: BlobRef) {
        let request = drive
            .admit_command(
                request_run_command(None, user_input(input_ref), run_config()),
                20,
            )
            .expect("request run");
        commit_action(drive, request);
    }

    fn request_run_command(
        submission_id: Option<crate::SubmissionId>,
        input: Vec<ContextEntryInput>,
        run_config: RunConfig,
    ) -> CoreAgentCommand {
        CoreAgentCommand::RequestRun(RunRequestCommand {
            notify_on_terminal: Vec::new(),
            submission_id,
            source: RunRequestSource::Input { input },
            run_config,
        })
    }

    fn user_input(input_ref: BlobRef) -> Vec<ContextEntryInput> {
        vec![message_input(ContextMessageRole::User, input_ref)]
    }

    fn message_input(role: ContextMessageRole, content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Message { role },
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn provider_opaque_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::ProviderOpaque,
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn provider_opaque_input_with_tokens(content_ref: BlobRef, tokens: u32) -> ContextEntryInput {
        let mut input = provider_opaque_input(content_ref);
        input.token_estimate = Some(TokenEstimate {
            tokens,
            quality: TokenEstimateQuality::Estimated,
        });
        input
    }

    fn openai_compaction_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::ProviderOpaque,
            content_ref,
            media_type: Some("application/json".to_owned()),
            preview: Some("OpenAI Responses compaction item".to_owned()),
            provider_kind: Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND.to_owned()),
            provider_item_id: Some("cmp_1".to_owned()),
            token_estimate: None,
        }
    }

    fn instruction_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Instructions,
            content_ref,
            media_type: Some("text/plain".to_owned()),
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn skill_catalog_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::SkillCatalog,
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn skill_activation_input(
        skill_id: SkillId,
        content_ref: BlobRef,
        provider_kind: Option<&str>,
    ) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::SkillActivation { skill_id },
            content_ref,
            media_type: Some("text/markdown".to_owned()),
            preview: None,
            provider_kind: provider_kind.map(str::to_owned),
            provider_item_id: None,
            token_estimate: None,
        }
    }

    fn drive_until_generate(drive: &mut CoreAgentDrive) -> LlmGenerationRequest {
        drive_until_generate_with_planned_event(drive).1
    }

    fn test_tool_spec(tool_name: &str) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(tool_name),
            kind: ToolKind::Function(FunctionToolSpec {
                model_name: None,
                description_ref: None,
                input_schema_ref: BlobRef::from_bytes(br#"{"type":"object"}"#),
                output_schema_ref: None,
                strict: None,
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: ToolTargetRequirement::None,
        }
    }

    fn install_test_tool(drive: &mut CoreAgentDrive, tool_name: &str) {
        let spec = test_tool_spec(tool_name);
        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(drive.state().tooling.revision),
                    tools: std::collections::BTreeMap::from([(spec.name.clone(), spec)]),
                },
                15,
            )
            .expect("replace tools");
        commit_action(drive, action);
    }

    fn drive_until_tool_batch_request(
        drive: &mut CoreAgentDrive,
        request: LlmGenerationRequest,
        tool_name: &str,
    ) -> ToolInvocationBatchRequest {
        let tool_call = ObservedToolCall {
            call_id: crate::ToolCallId::new("call_wait"),
            tool_name: ToolName::new(tool_name),
            provider_kind: None,
            arguments_ref: BlobRef::from_bytes(br#"{"wait":true}"#),
            native_call_ref: None,
        };
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-tool".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: vec![tool_call],
                        context_token_estimate: None,
                    },
                },
                80,
            )
            .expect("resume generation");
        commit_action(drive, resumed);

        for observed_at_ms in 81..120 {
            let action = drive.next_action(observed_at_ms, 64).expect("next action");
            if let CoreAgentAction::InvokeTools { request } = action {
                return request;
            }
            commit_action(drive, action);
        }
        panic!("drive did not emit a tool invocation");
    }

    fn drive_to_single_tool_invocation(drive: &mut CoreAgentDrive) -> ToolInvocationBatchRequest {
        open_session(drive);
        install_test_tool(drive, "await");
        request_run(drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(drive);
        drive_until_tool_batch_request(drive, request, "await")
    }

    fn completed_tool_result(request: &ToolInvocationBatchRequest) -> ToolInvocationBatchResult {
        ToolInvocationBatchResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            results: vec![ToolInvocationResult {
                call_id: request.calls[0].call_id.clone(),
                status: ToolCallStatus::Succeeded,
                output_ref: Some(BlobRef::from_bytes(b"wait completed")),
                model_visible_context_entries: vec![
                    ToolInvocationResult::tool_result_context_entry(
                        &request.calls[0].call_id,
                        ToolCallStatus::Succeeded,
                        BlobRef::from_bytes(b"wait completed"),
                    ),
                ],
                error_ref: None,
                effects: vec![ToolEffect {
                    kind: "test".to_owned(),
                    data: Default::default(),
                }],
            }],
        }
    }

    fn wait_await_spec() -> AwaitSpec {
        AwaitSpec {
            promise_ids: Vec::new(),
            mode: AwaitMode::All,
            deadline_at_ms: Some(90),
            mailbox: true,
        }
    }

    fn deferred_await_outcome(request: &ToolInvocationBatchRequest) -> ToolBatchOutcome {
        deferred_await_outcome_with_spec(request, wait_await_spec())
    }

    fn deferred_await_outcome_with_spec(
        request: &ToolInvocationBatchRequest,
        spec: AwaitSpec,
    ) -> ToolBatchOutcome {
        ToolBatchOutcome::Deferred {
            batch_id: request.batch_id,
            call_id: request.calls[0].call_id.clone(),
            completed_results: Vec::new(),
            spec,
        }
    }

    fn resume_await_command(request: &ToolInvocationBatchRequest) -> CoreAgentCommand {
        resume_await_command_with_claim(request, WakeReason::Timeout)
    }

    fn resume_await_command_with_claim(
        request: &ToolInvocationBatchRequest,
        claim: WakeReason,
    ) -> CoreAgentCommand {
        CoreAgentCommand::ResumeAwait(crate::ResumeAwaitCommand {
            run_id: request.run_id,
            batch_id: request.batch_id,
            claim,
            claim_observed_at_ms: 91,
            output: AwaitOutputRefs {
                output_ref: BlobRef::from_bytes(b"await output"),
                summary_ref: BlobRef::from_bytes(b"await summary"),
            },
        })
    }

    fn drive_until_generate_with_planned_event(
        drive: &mut CoreAgentDrive,
    ) -> (TurnEvent, LlmGenerationRequest) {
        let mut planned = None;
        for observed_at_ms in 21..80 {
            let action = drive.next_action(observed_at_ms, 64).expect("next action");
            if let CoreAgentAction::GenerateLlm { request } = action {
                return (
                    planned.expect("drive emitted generation without planned event"),
                    request,
                );
            }
            let entries = commit_action(drive, action);
            for entry in entries {
                if let CoreAgentEvent::Turn(event @ TurnEvent::Planned { .. }) = entry.event {
                    planned = Some(event);
                }
            }
        }
        panic!("drive did not emit an LLM action");
    }

    fn openai_items(request: &LlmGenerationRequest) -> &[ContextEntry] {
        &request.request.context.entries
    }

    fn planned_event_size_for_context_entry_count(count: usize) -> (usize, usize) {
        let session_id = SessionId::new(format!("session-context-{count}"));
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        for index in 0..count {
            let action = drive
                .admit_command(
                    CoreAgentCommand::UpsertContext {
                        expected_revision: None,
                        key: ContextEntryKey::new(format!("context.entry.{index:04}")),
                        entry: provider_opaque_input(BlobRef::from_bytes(
                            format!("context entry {index}").as_bytes(),
                        )),
                    },
                    20 + index as u64,
                )
                .expect("context edit");
            commit_action(&mut drive, action);
        }
        request_run(&mut drive, BlobRef::from_bytes(b"user"));

        let (planned, request) = drive_until_generate_with_planned_event(&mut drive);
        let planned_size = serde_json::to_vec(&planned)
            .expect("serialize planned event")
            .len();
        (planned_size, request.request.context.entries.len())
    }

    #[test]
    fn planned_turn_event_and_state_store_metadata_only() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));

        let (planned, request) = drive_until_generate_with_planned_event(&mut drive);
        let TurnEvent::Planned {
            ref request_fingerprint,
            config_revision,
            context_revision,
            toolset_revision,
            ..
        } = planned
        else {
            panic!("expected planned event");
        };
        assert_eq!(request_fingerprint, &request.request.request_fingerprint);
        assert_eq!(config_revision, drive.state().lifecycle.config_revision);
        assert_eq!(context_revision, request.request.context.context_revision);
        assert_eq!(toolset_revision, drive.state().tooling.revision);

        let planned_json = serde_json::to_value(&planned).expect("serialize planned event");
        let planned_object = planned_json
            .get("planned")
            .and_then(serde_json::Value::as_object)
            .expect("planned event object");
        assert!(!planned_object.contains_key("request"));
        assert!(!planned_object.contains_key("context"));
        assert!(!planned_object.contains_key("tools"));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let active_turn = active_run.turns.get(&request.turn_id).expect("active turn");
        assert!(active_turn.planned_request.is_some());
        let turn_state = serde_json::to_value(active_turn).expect("serialize turn state");
        let turn_object = turn_state.as_object().expect("turn state object");
        assert!(!turn_object.contains_key("request"));
        assert!(turn_object.contains_key("planned_request"));
    }

    #[test]
    fn planned_request_rebuilds_from_durable_events() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let mut entries = Vec::new();

        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        entries.extend(commit_action(&mut drive, open));
        let request_run = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    run_config(),
                ),
                20,
            )
            .expect("request run");
        entries.extend(commit_action(&mut drive, request_run));

        let request = loop {
            let action = drive.next_action(30, 64).expect("next action");
            if let CoreAgentAction::GenerateLlm { request } = action {
                break request;
            }
            entries.extend(commit_action(&mut drive, action));
        };

        let rebuilt = rebuild_llm_request_for_planned_turn(&entries, request.turn_id)
            .expect("rebuild request")
            .expect("planned request exists");
        assert_eq!(rebuilt, request.request);
    }

    #[test]
    fn planned_turn_event_size_does_not_scale_with_active_context() {
        let (small_event_size, small_context_len) = planned_event_size_for_context_entry_count(1);
        let (large_event_size, large_context_len) = planned_event_size_for_context_entry_count(80);

        assert!(large_context_len > small_context_len + 70);
        assert!(
            large_event_size.abs_diff(small_event_size) < 32,
            "planned event size should be fixed-ish: small={small_event_size} large={large_event_size}"
        );
    }

    #[test]
    fn request_run_rejects_non_user_message_input() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                request_run_command(
                    None,
                    vec![message_input(
                        ContextMessageRole::Assistant,
                        BlobRef::from_bytes(b"assistant"),
                    )],
                    run_config(),
                ),
                20,
            )
            .expect_err("assistant run input must be rejected");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
        assert!(rejection.message.contains("run input cannot supply"));
    }

    #[test]
    fn request_run_accepts_provider_opaque_native_input() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let action = drive
            .admit_command(
                request_run_command(
                    None,
                    vec![provider_opaque_input(BlobRef::from_bytes(b"native"))],
                    run_config(),
                ),
                20,
            )
            .expect("provider-opaque run input");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().runs.queued.len(), 1);
        assert!(matches!(
            drive.state().runs.queued[0].source.input()[0].kind,
            ContextEntryKind::ProviderOpaque
        ));
    }

    #[test]
    fn upsert_context_accepts_provider_opaque_entry() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let key = ContextEntryKey::new("client.native");

        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: key.clone(),
                    entry: provider_opaque_input(BlobRef::from_bytes(b"native")),
                },
                20,
            )
            .expect("provider-opaque context edit");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().context.entries.len(), 1);
        let entry = &drive.state().context.entries[0];
        assert_eq!(entry.key.as_ref(), Some(&key));
        assert!(matches!(entry.kind, ContextEntryKind::ProviderOpaque));
        assert!(matches!(entry.source, ContextEntrySource::ContextEdit));
    }

    #[test]
    fn stale_context_revision_rejects_all_direct_edits_with_structured_details() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let key = ContextEntryKey::new("client.native");
        let input = provider_opaque_input(BlobRef::from_bytes(b"native"));

        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: key.clone(),
                    entry: input.clone(),
                },
                20,
            )
            .expect("initial context edit");
        commit_action(&mut drive, action);
        assert_eq!(drive.state().context.revision, 1);

        let replacement = std::collections::BTreeMap::from([(key.clone(), input.clone())]);
        let commands = [
            CoreAgentCommand::UpsertContext {
                expected_revision: Some(0),
                key: key.clone(),
                entry: input,
            },
            CoreAgentCommand::ReplaceContextPrefix {
                expected_revision: Some(0),
                key_prefix: ContextEntryKey::new("client"),
                entries: replacement,
            },
            CoreAgentCommand::RemoveContext {
                expected_revision: Some(0),
                key,
            },
        ];

        for command in commands {
            let error = drive
                .admit_command(command, 30)
                .expect_err("stale context edit must be rejected");
            let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error
            else {
                panic!("expected rejected command");
            };
            assert_eq!(rejection.kind, CommandRejectionKind::RevisionConflict);
            assert_eq!(
                rejection.details,
                Some(CommandRejectionDetails::ContextRevisionConflict {
                    expected: 0,
                    actual: 1,
                })
            );
        }

        assert_eq!(drive.state().context.revision, 1);
        assert_eq!(drive.state().context.entries.len(), 1);
    }

    #[test]
    fn upsert_context_rejects_reserved_run_key_prefix() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("run.1.input.0"),
                    entry: message_input(ContextMessageRole::User, BlobRef::from_bytes(b"bad")),
                },
                20,
            )
            .expect_err("reserved run context key must be rejected");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
        assert!(rejection.message.contains("reserved internal prefix"));
    }

    #[test]
    fn remove_context_rejects_reserved_run_key_prefix() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                CoreAgentCommand::RemoveContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("run.1.input.0"),
                },
                20,
            )
            .expect_err("reserved run context key must be rejected");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
        assert!(rejection.message.contains("reserved internal prefix"));
    }

    #[test]
    fn context_source_run_acceptance_records_resolved_trigger_entry_ids() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let key = ContextEntryKey::new("client.message.1");

        let upsert = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: key.clone(),
                    entry: message_input(ContextMessageRole::User, BlobRef::from_bytes(b"hello")),
                },
                20,
            )
            .expect("context upsert");
        commit_action(&mut drive, upsert);
        let entry_id = drive.state().context.entries[0].entry_id;

        let request = drive
            .admit_command(
                CoreAgentCommand::RequestRun(RunRequestCommand {
                    notify_on_terminal: Vec::new(),
                    submission_id: None,
                    source: RunRequestSource::Context {
                        keys: vec![key.clone()],
                    },
                    run_config: run_config(),
                }),
                30,
            )
            .expect("context source run");
        commit_action(&mut drive, request);

        let crate::RunSource::Context { triggers } = &drive.state().runs.queued[0].source else {
            panic!("expected context source");
        };
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].key, key);
        assert_eq!(triggers[0].entry_id, entry_id);
    }

    #[test]
    fn upsert_context_accepts_instruction_entry_with_instruction_key() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let key = ContextEntryKey::new("instructions.100.base");

        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: key.clone(),
                    entry: instruction_input(BlobRef::from_bytes(b"base instructions")),
                },
                20,
            )
            .expect("instruction context edit");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().context.entries.len(), 1);
        let entry = &drive.state().context.entries[0];
        assert_eq!(entry.key.as_ref(), Some(&key));
        assert!(matches!(entry.kind, ContextEntryKind::Instructions));
    }

    #[test]
    fn replace_context_prefix_syncs_managed_instruction_entries() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        for (key, content) in [
            ("instructions.000.default", b"default".as_slice()),
            ("instructions.100.prompts.old", b"old prompt".as_slice()),
            ("instructions.100.promptsettings", b"adjacent".as_slice()),
            ("instructions.200.other", b"other".as_slice()),
        ] {
            let action = drive
                .admit_command(
                    CoreAgentCommand::UpsertContext {
                        expected_revision: None,
                        key: ContextEntryKey::new(key),
                        entry: instruction_input(BlobRef::from_bytes(content)),
                    },
                    20,
                )
                .expect("instruction context edit");
            commit_action(&mut drive, action);
        }

        let first_ref = BlobRef::from_bytes(b"first prompt");
        let second_ref = BlobRef::from_bytes(b"second prompt");
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            ContextEntryKey::new("instructions.100.prompts.0000.base"),
            instruction_input(first_ref.clone()),
        );
        entries.insert(
            ContextEntryKey::new("instructions.100.prompts.0001.style"),
            instruction_input(second_ref.clone()),
        );
        let before_revision = drive.state().context.revision;

        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceContextPrefix {
                    expected_revision: None,
                    key_prefix: ContextEntryKey::new("instructions.100.prompts"),
                    entries,
                },
                20,
            )
            .expect("replace prompt prefix");
        let events = commit_action(&mut drive, action);

        assert_eq!(events.len(), 1);
        assert_eq!(drive.state().context.revision, before_revision + 1);
        let keys = drive
            .state()
            .context
            .entries
            .iter()
            .filter_map(|entry| entry.key.as_ref().map(|key| key.as_str().to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "instructions.000.default",
                "instructions.100.promptsettings",
                "instructions.200.other",
                "instructions.100.prompts.0000.base",
                "instructions.100.prompts.0001.style"
            ]
        );
        assert!(drive.state().context.entries.iter().all(|entry| {
            entry
                .key
                .as_ref()
                .is_none_or(|key| key.as_str() != "instructions.100.prompts.old")
        }));
    }

    #[test]
    fn replace_context_prefix_rejects_entries_outside_prefix() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            ContextEntryKey::new("instructions.200.other"),
            instruction_input(BlobRef::from_bytes(b"other")),
        );

        let error = drive
            .admit_command(
                CoreAgentCommand::ReplaceContextPrefix {
                    expected_revision: None,
                    key_prefix: ContextEntryKey::new("instructions.100.prompts"),
                    entries,
                },
                20,
            )
            .expect_err("entry outside prefix must be rejected");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
        assert!(rejection.message.contains("outside prefix"));
    }

    #[test]
    fn upsert_context_rejects_instruction_entry_without_instruction_key() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.instructions"),
                    entry: instruction_input(BlobRef::from_bytes(b"base instructions")),
                },
                20,
            )
            .expect_err("instruction context edit must use instruction key");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
        assert!(
            rejection
                .message
                .contains("instruction context entry requires")
        );
    }

    #[test]
    fn upsert_context_accepts_user_message_entry_and_dedupes_replays() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let content_ref = BlobRef::from_bytes(b"persistent user message");

        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("channel.room.batch-1"),
                    entry: message_input(ContextMessageRole::User, content_ref.clone()),
                },
                20,
            )
            .expect("user-message context edit must be accepted");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().context.entries.len(), 1);
        let entry = &drive.state().context.entries[0];
        assert_eq!(
            entry.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            }
        );
        assert_eq!(entry.content_ref, content_ref);
        assert_eq!(entry.source, ContextEntrySource::ContextEdit);
        let revision = drive.state().context.revision;

        let replay = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("channel.room.batch-1"),
                    entry: message_input(ContextMessageRole::User, content_ref),
                },
                30,
            )
            .expect("identical upsert replay must be admitted as a no-op");
        assert!(
            !matches!(replay, CoreAgentAction::AppendEvents { .. }),
            "identical upsert replay must produce no events, got {replay:?}"
        );
        assert_eq!(drive.state().context.entries.len(), 1);
        assert_eq!(drive.state().context.revision, revision);
    }

    #[test]
    fn upsert_context_rejects_assistant_message_entry() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.message"),
                    entry: message_input(
                        ContextMessageRole::Assistant,
                        BlobRef::from_bytes(b"forged assistant message"),
                    ),
                },
                20,
            )
            .expect_err("assistant-message context edit must be rejected");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
        assert!(rejection.message.contains("context edit cannot supply"));
        assert!(drive.state().context.entries.is_empty());
    }

    #[test]
    fn planned_context_includes_instruction_entries_first_by_key() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let second_ref = BlobRef::from_bytes(b"second instructions");
        let first_ref = BlobRef::from_bytes(b"first instructions");
        let input_ref = BlobRef::from_bytes(b"user input");

        for (key, content_ref) in [
            ("instructions.200.second", second_ref.clone()),
            ("instructions.100.first", first_ref.clone()),
        ] {
            let action = drive
                .admit_command(
                    CoreAgentCommand::UpsertContext {
                        expected_revision: None,
                        key: ContextEntryKey::new(key),
                        entry: instruction_input(content_ref),
                    },
                    20,
                )
                .expect("instruction context edit");
            commit_action(&mut drive, action);
        }
        request_run(&mut drive, input_ref.clone());

        let request = drive_until_generate(&mut drive);
        let items = openai_items(&request);

        assert_eq!(items.len(), 3);
        assert!(matches!(items[0].kind, ContextEntryKind::Instructions));
        assert_eq!(items[0].content_ref, first_ref);
        assert!(matches!(items[1].kind, ContextEntryKind::Instructions));
        assert_eq!(items[1].content_ref, second_ref);
        assert!(matches!(
            items[2].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(items[2].content_ref, input_ref);
    }

    #[test]
    fn queued_run_input_does_not_enter_context_until_run_starts() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        request_run(&mut drive, BlobRef::from_bytes(b"input"));

        assert_eq!(drive.state().runs.queued.len(), 1);
        assert!(drive.state().runs.active.is_none());
        assert!(drive.state().context.entries.is_empty());
        assert_eq!(drive.state().context.revision, 0);
    }

    #[test]
    fn run_input_materializes_once_before_turn_planning() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));

        let start_run = drive.next_action(21, 64).expect("start run");
        commit_action(&mut drive, start_run);
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert!(active_run.input_entry_ids.is_empty());
        assert!(drive.state().context.entries.is_empty());

        let materialize_input = drive.next_action(22, 64).expect("materialize input");
        let entries = commit_action(&mut drive, materialize_input);
        let CoreAgentEvent::Context(ContextEvent::EntriesApplied {
            entries: applied, ..
        }) = &entries[0].event
        else {
            panic!("expected context entries");
        };
        assert_eq!(applied.len(), 1);
        assert!(matches!(
            applied[0].source,
            ContextEntrySource::RunInput { input_index: 0, .. }
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(active_run.input_entry_ids, vec![applied[0].entry_id]);
        assert_eq!(active_run.input_consumed_by_turn_id, None);
        assert_eq!(drive.state().context.entries.len(), 1);

        let start_turn = drive.next_action(23, 64).expect("start turn");
        let entries = commit_action(&mut drive, start_turn);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Turn(TurnEvent::Started { .. })
        ));
        assert_eq!(drive.state().context.entries.len(), 1);
    }

    #[test]
    fn unconsumed_run_input_context_cannot_be_removed() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let start_run = drive.next_action(21, 64).expect("start run");
        commit_action(&mut drive, start_run);
        let materialize_input = drive.next_action(22, 64).expect("materialize input");
        commit_action(&mut drive, materialize_input);

        let entry_id = drive
            .state()
            .runs
            .active
            .as_ref()
            .expect("active run")
            .input_entry_ids[0];
        let base_revision = drive.state().context.revision;
        let error = commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
                base_revision,
                entry_ids: vec![entry_id],
                reason: ContextRemovalReason::Pruned,
            }),
            30,
        )
        .expect_err("unconsumed run input removal must fail");

        assert!(matches!(error, CoreAgentDriveError::Domain(_)));
        assert_eq!(drive.state().context.entries.len(), 1);
    }

    #[test]
    fn consumed_run_input_context_can_be_removed() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(&mut drive);

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let entry_id = active_run.input_entry_ids[0];
        assert_eq!(active_run.input_consumed_by_turn_id, Some(request.turn_id));

        let base_revision = drive.state().context.revision;
        commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
                base_revision,
                entry_ids: vec![entry_id],
                reason: ContextRemovalReason::Pruned,
            }),
            30,
        )
        .expect("consumed run input removal");

        assert!(drive.state().context.entries.is_empty());
    }

    #[test]
    fn provider_compaction_prunes_superseded_entries_after_compaction_item() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input before compaction"));
        let llm_request = drive_until_generate(&mut drive);
        let consumed_input_entry_id = drive
            .state()
            .runs
            .active
            .as_ref()
            .expect("active run")
            .input_entry_ids[0];

        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: llm_request.run_id,
                    turn_id: llm_request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![
                        openai_compaction_input(BlobRef::from_bytes(
                            br#"{"type":"compaction","encrypted_content":"opaque"}"#,
                        )),
                        message_input(
                            ContextMessageRole::Assistant,
                            BlobRef::from_bytes(b"assistant after compaction"),
                        ),
                    ],
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .expect("resume generation");
        commit_action(&mut drive, resumed);

        let complete_run = drive.next_action(31, 64).expect("complete run");
        commit_action(&mut drive, complete_run);

        let prune = drive
            .next_action(32, 64)
            .expect("provider compaction prune");
        let entries = commit_action(&mut drive, prune);
        let CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
            entry_ids, reason, ..
        }) = &entries[0].event
        else {
            panic!("expected context removal");
        };
        assert_eq!(entry_ids, &vec![consumed_input_entry_id]);
        assert_eq!(reason, &ContextRemovalReason::ProviderCompacted);

        let retained = &drive.state().context.entries;
        assert_eq!(retained.len(), 2);
        assert!(matches!(retained[0].kind, ContextEntryKind::ProviderOpaque));
        assert_eq!(
            retained[0].provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
        assert!(matches!(
            retained[1].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant
            }
        ));
    }

    #[test]
    fn manual_standalone_compaction_emits_provider_request_and_prunes_replaced_entries() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(None, Some(256)));
        let context_ref = BlobRef::from_bytes(b"native context");
        let upsert = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.native"),
                    entry: provider_opaque_input(context_ref.clone()),
                },
                20,
            )
            .expect("context edit");
        commit_action(&mut drive, upsert);
        let original_entry_id = drive.state().context.entries[0].entry_id;

        let request_compaction = drive
            .admit_command(CoreAgentCommand::CompactContext, 30)
            .expect("manual compaction");
        let requested_entries = commit_action(&mut drive, request_compaction);
        let CoreAgentEvent::Context(ContextEvent::CompactionRequested { trigger, .. }) =
            &requested_entries[0].event
        else {
            panic!("expected compaction request");
        };
        assert_eq!(trigger, &ContextCompactionTrigger::Manual);

        let CoreAgentAction::CompactContext { request } =
            drive.next_action(31, 64).expect("compact action")
        else {
            panic!("expected compact action");
        };
        assert_eq!(request.session_id, session_id);
        let compaction_task = &request.request;
        assert_eq!(compaction_task.target_tokens, Some(256));
        assert_eq!(compaction_task.context.entry_ids(), vec![original_entry_id]);
        assert_eq!(compaction_task.context.context_revision, 2);

        let completed = drive
            .resume_context_compaction(
                ContextCompactionResult {
                    session_id: request.session_id,
                    context_revision: compaction_task.context.context_revision,
                    status: ContextCompactionStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![openai_compaction_input(BlobRef::from_bytes(
                        br#"{"type":"compaction","encrypted_content":"opaque"}"#,
                    ))],
                },
                32,
            )
            .expect("resume compaction");
        let completed_entries = commit_action(&mut drive, completed);
        assert!(matches!(
            completed_entries[0].event,
            CoreAgentEvent::Context(ContextEvent::EntriesApplied { .. })
        ));
        assert!(matches!(
            completed_entries[1].event,
            CoreAgentEvent::Context(ContextEvent::CompactionFinished {
                status: ContextCompactionStatus::Succeeded,
                ..
            })
        ));
        assert!(!drive.state().context.pending_compaction);

        let prune = drive.next_action(33, 64).expect("prune compacted entries");
        let pruned_entries = commit_action(&mut drive, prune);
        let CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
            entry_ids, reason, ..
        }) = &pruned_entries[0].event
        else {
            panic!("expected provider compaction prune");
        };
        assert_eq!(entry_ids, &vec![original_entry_id]);
        assert_eq!(reason, &ContextRemovalReason::ProviderCompacted);
        assert_eq!(drive.state().context.entries.len(), 1);
        assert_eq!(
            drive.state().context.entries[0].provider_kind.as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
    }

    #[test]
    fn failed_manual_standalone_compaction_clears_pending_state() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(None, Some(256)));
        let upsert = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.native"),
                    entry: provider_opaque_input(BlobRef::from_bytes(b"native context")),
                },
                20,
            )
            .expect("context edit");
        commit_action(&mut drive, upsert);

        let request_compaction = drive
            .admit_command(CoreAgentCommand::CompactContext, 30)
            .expect("manual compaction");
        commit_action(&mut drive, request_compaction);

        let CoreAgentAction::CompactContext { request } =
            drive.next_action(31, 64).expect("compact action")
        else {
            panic!("expected compact action");
        };
        let compaction_task = &request.request;
        let failure_ref = BlobRef::from_bytes(b"compact failed");
        let completed = drive
            .resume_context_compaction(
                ContextCompactionResult {
                    session_id,
                    context_revision: compaction_task.context.context_revision,
                    status: ContextCompactionStatus::Failed,
                    failure_ref: Some(failure_ref.clone()),
                    context_entries: Vec::new(),
                },
                32,
            )
            .expect("resume failed compaction");
        let completed_entries = commit_action(&mut drive, completed);

        let CoreAgentEvent::Context(ContextEvent::CompactionFinished {
            status,
            failure_ref: event_failure_ref,
            ..
        }) = &completed_entries[0].event
        else {
            panic!("expected compaction finished");
        };
        assert_eq!(status, &ContextCompactionStatus::Failed);
        assert_eq!(event_failure_ref.as_ref(), Some(&failure_ref));
        assert!(!drive.state().context.pending_compaction);
        assert!(matches!(
            drive.next_action(33, 64).expect("next action"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn pending_standalone_compaction_blocks_context_mutations_and_runs() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(None, Some(256)));
        let upsert = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.native"),
                    entry: provider_opaque_input(BlobRef::from_bytes(b"native context")),
                },
                20,
            )
            .expect("context edit");
        commit_action(&mut drive, upsert);

        let request_compaction = drive
            .admit_command(CoreAgentCommand::CompactContext, 30)
            .expect("manual compaction");
        commit_action(&mut drive, request_compaction);

        let run_error = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"new work")),
                    run_config(),
                ),
                31,
            )
            .expect_err("run should be rejected while compaction is pending");
        assert!(matches!(
            run_error,
            CoreAgentDriveError::Command(CommandError::Rejected(ref rejection))
                if rejection.kind == CommandRejectionKind::ActiveWork
        ));

        let edit_error = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.native.2"),
                    entry: provider_opaque_input(BlobRef::from_bytes(b"changed context")),
                },
                32,
            )
            .expect_err("context edit should be rejected while compaction is pending");
        assert!(matches!(
            edit_error,
            CoreAgentDriveError::Command(CommandError::Rejected(ref rejection))
                if rejection.kind == CommandRejectionKind::ActiveWork
        ));
    }

    #[test]
    fn high_watermark_standalone_compaction_requests_provider_call_when_idle() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(Some(10), Some(4)));

        for (index, tokens) in [6, 5].into_iter().enumerate() {
            let upsert = drive
                .admit_command(
                    CoreAgentCommand::UpsertContext {
                        expected_revision: None,
                        key: ContextEntryKey::new(format!("client.native.{index}")),
                        entry: provider_opaque_input_with_tokens(
                            BlobRef::from_bytes(format!("native {index}").as_bytes()),
                            tokens,
                        ),
                    },
                    20 + index as u64,
                )
                .expect("context edit");
            commit_action(&mut drive, upsert);
        }
        let entry_ids = drive
            .state()
            .context
            .entries
            .iter()
            .map(|entry| entry.entry_id)
            .collect::<Vec<_>>();

        let action = drive.next_action(30, 64).expect("high watermark plan");
        let requested_entries = commit_action(&mut drive, action);
        let CoreAgentEvent::Context(ContextEvent::CompactionRequested { trigger, .. }) =
            &requested_entries[0].event
        else {
            panic!("expected compaction request");
        };
        assert_eq!(trigger, &ContextCompactionTrigger::HighWatermark);

        let CoreAgentAction::CompactContext { request } =
            drive.next_action(31, 64).expect("compact action")
        else {
            panic!("expected compact action");
        };
        assert_eq!(request.session_id, session_id);
        let compaction_task = &request.request;
        assert_eq!(compaction_task.target_tokens, Some(4));
        assert_eq!(compaction_task.context.entry_ids(), entry_ids);
        assert_eq!(
            compaction_task
                .context
                .token_estimate
                .as_ref()
                .map(|estimate| estimate.tokens),
            Some(11)
        );
    }

    #[test]
    fn high_watermark_standalone_compaction_uses_compactable_context_estimate() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(Some(10), Some(4)));

        let instructions = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("instructions.100.base"),
                    entry: instruction_input(BlobRef::from_bytes(b"base instructions")),
                },
                20,
            )
            .expect("instruction edit");
        commit_action(&mut drive, instructions);
        let context = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.native"),
                    entry: provider_opaque_input_with_tokens(
                        BlobRef::from_bytes(b"native context"),
                        11,
                    ),
                },
                21,
            )
            .expect("context edit");
        commit_action(&mut drive, context);

        let action = drive.next_action(30, 64).expect("high watermark plan");
        let requested_entries = commit_action(&mut drive, action);
        assert!(matches!(
            requested_entries[0].event,
            CoreAgentEvent::Context(ContextEvent::CompactionRequested {
                trigger: ContextCompactionTrigger::HighWatermark,
                ..
            })
        ));

        let CoreAgentAction::CompactContext { request } =
            drive.next_action(31, 64).expect("compact action")
        else {
            panic!("expected compact action");
        };
        assert_eq!(request.session_id, session_id);
        let compaction_task = &request.request;
        assert_eq!(
            compaction_task.context.entries.len(),
            1,
            "instructions are preserved outside the compactable provider window"
        );
        assert!(matches!(
            compaction_task.context.entries[0].kind,
            ContextEntryKind::ProviderOpaque
        ));
        assert_eq!(
            compaction_task
                .context
                .token_estimate
                .as_ref()
                .map(|estimate| estimate.tokens),
            Some(11)
        );
    }

    #[test]
    fn stale_context_base_revision_is_rejected() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let start_run = drive.next_action(21, 64).expect("start run");
        commit_action(&mut drive, start_run);
        let materialize_input = drive.next_action(22, 64).expect("materialize input");
        commit_action(&mut drive, materialize_input);

        let entry_id = drive.state().context.entries[0].entry_id;
        assert_eq!(drive.state().context.revision, 1);
        let error = commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
                base_revision: 0,
                entry_ids: vec![entry_id],
                reason: ContextRemovalReason::Pruned,
            }),
            30,
        )
        .expect_err("stale base revision must fail");

        assert!(matches!(error, CoreAgentDriveError::Domain(_)));
        assert_eq!(drive.state().context.entries.len(), 1);
    }

    #[test]
    fn duplicate_key_entries_in_one_context_event_are_rejected() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let key = ContextEntryKey::new("client.note");
        let base_revision = drive.state().context.revision;

        let error = commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision,
                entries: vec![
                    context_edit_entry(1, Some(key.clone()), b"first"),
                    context_edit_entry(2, Some(key), b"second"),
                ],
            }),
            20,
        )
        .expect_err("duplicate keys must fail");

        assert!(matches!(error, CoreAgentDriveError::Domain(_)));
        assert!(drive.state().context.entries.is_empty());
        assert_eq!(drive.state().id_cursors.last_context_item_id, 0);
    }

    #[test]
    fn missing_context_key_removal_is_rejected_at_admission() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                CoreAgentCommand::RemoveContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("client.note"),
                },
                20,
            )
            .expect_err("missing key removal must fail");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::UnknownReference);
    }

    #[test]
    fn state_replacement_cannot_introduce_new_context_entries() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let base_revision = drive.state().context.revision;

        let error = commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Context(ContextEvent::StateReplaced {
                base_revision,
                entries: vec![context_edit_entry(1, None, b"new")],
                reason: ContextRewriteReason::PolicyChanged,
            }),
            20,
        )
        .expect_err("replacement cannot introduce new entries");

        assert!(matches!(error, CoreAgentDriveError::Domain(_)));
        assert!(drive.state().context.entries.is_empty());
    }

    #[test]
    fn steering_materializes_after_in_flight_turn_snapshot() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(&mut drive);
        assert_eq!(openai_items(&request).len(), 1);

        let steering_one = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    input: user_input(BlobRef::from_bytes(b"steering one")),
                },
                30,
            )
            .expect("steering one");
        commit_action(&mut drive, steering_one);
        let steering_two = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    input: user_input(BlobRef::from_bytes(b"steering two")),
                },
                31,
            )
            .expect("steering two");
        commit_action(&mut drive, steering_two);

        let materialize_steering = drive.next_action(32, 64).expect("materialize steering");
        let entries = commit_action(&mut drive, materialize_steering);
        let CoreAgentEvent::Context(ContextEvent::EntriesApplied {
            entries: applied, ..
        }) = &entries[0].event
        else {
            panic!("expected context entries");
        };
        assert_eq!(applied.len(), 2);
        assert!(matches!(
            applied[0].source,
            ContextEntrySource::Steering {
                steering_id,
                input_index: 0,
                ..
            } if steering_id.as_u64() == 1
        ));
        assert!(matches!(
            applied[1].source,
            ContextEntrySource::Steering {
                steering_id,
                input_index: 0,
                ..
            } if steering_id.as_u64() == 2
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(active_run.steering.len(), 2);
        assert_eq!(active_run.steering[0].entry_ids, vec![applied[0].entry_id]);
        assert_eq!(active_run.steering[1].entry_ids, vec![applied[1].entry_id]);
        assert_eq!(active_run.steering[0].consumed_by_turn_id, None);
        assert_eq!(active_run.steering[1].consumed_by_turn_id, None);

        let active_turn = active_run.turns.get(&request.turn_id).expect("active turn");
        assert_eq!(active_turn.status, TurnStatus::GenerationPending);
        let planned_request = active_turn
            .planned_request
            .as_ref()
            .expect("planned request metadata");
        assert_eq!(
            planned_request.context_revision,
            request.request.context.context_revision
        );
        assert_eq!(request.request.context.entries.len(), 1);
    }

    #[test]
    fn drive_emits_append_action_after_command_admission() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);

        let action = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("admit command");

        assert!(matches!(action, CoreAgentAction::AppendEvents { .. }));
        assert_eq!(drive.state().lifecycle.status, CoreAgentStatus::New);
    }

    #[test]
    fn drive_applies_only_committed_appended_entries() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let action = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("admit command");

        assert_eq!(drive.state().lifecycle.status, CoreAgentStatus::New);
        let entries = commit_action(&mut drive, action);

        assert_eq!(entries.len(), 1);
        assert_eq!(drive.state().lifecycle.status, CoreAgentStatus::Open);
    }

    #[test]
    fn replace_session_config_updates_full_config_snapshot() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let mut next = config();
        next.generation.max_output_tokens = Some(2048);
        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: Some(0),
                    config: next,
                },
                20,
            )
            .expect("replace config");
        commit_action(&mut drive, action);

        let config = drive
            .state()
            .lifecycle
            .config
            .as_ref()
            .expect("session config");
        assert_eq!(drive.state().lifecycle.config_revision, 1);
        assert_eq!(config.generation.max_output_tokens, Some(2048));
    }

    #[test]
    fn replace_session_config_with_identical_document_is_a_noop() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: Some(0),
                    config: config(),
                },
                20,
            )
            .expect("identical replace admits as no-op");

        assert!(matches!(action, CoreAgentAction::Idle));
        assert_eq!(drive.state().lifecycle.config_revision, 0);
    }

    #[test]
    fn replace_session_config_rejects_specific_tool_choice_for_missing_tool() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let mut next = config();
        next.generation.tool_choice = Some(ToolChoice::Specific {
            tool_name: ToolName::new("missing_tool"),
        });

        let error = drive
            .admit_command(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: Some(0),
                    config: next,
                },
                20,
            )
            .expect_err("replace must reject missing specific tool choice");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
    }

    #[test]
    fn request_run_rejects_specific_tool_choice_for_missing_tool() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);
        let mut run_config = run_config();
        run_config.tool_choice = Some(ToolChoice::Specific {
            tool_name: ToolName::new("missing_tool"),
        });

        let error = drive
            .admit_command(
                request_run_command(None, user_input(BlobRef::from_bytes(b"input")), run_config),
                20,
            )
            .expect_err("run must reject missing specific tool choice");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
    }

    #[test]
    fn replace_session_config_rejects_queued_work() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);
        let request = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    run_config(),
                ),
                20,
            )
            .expect("request run");
        commit_action(&mut drive, request);

        let error = drive
            .admit_command(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: Some(0),
                    config: config(),
                },
                30,
            )
            .expect_err("replace must reject queued work");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::ActiveWork);
    }

    #[test]
    fn skill_activation_context_edit_updates_context_without_starting_run() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let skill_id = SkillId::new("skill-1");
        let context_ref = BlobRef::from_bytes(b"skill body");
        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: skill_activation_context_key(&skill_id),
                    entry: skill_activation_input(skill_id.clone(), context_ref.clone(), None),
                },
                20,
            )
            .expect("set skill activation context");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().context.entries.len(), 1);
        assert!(matches!(
            &drive.state().context.entries[0].kind,
            ContextEntryKind::SkillActivation { skill_id: planned } if planned == &skill_id
        ));
        assert_eq!(drive.state().context.entries[0].content_ref, context_ref);
        assert!(drive.state().runs.active.is_none());
        assert!(drive.state().runs.queued.is_empty());
        assert!(matches!(
            drive.next_action(30, 8).expect("next action"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn skill_activation_context_key_must_match_entry_skill_id() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let error = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: skill_activation_context_key(&SkillId::new("skill-1")),
                    entry: skill_activation_input(
                        SkillId::new("skill-2"),
                        BlobRef::from_bytes(b"skill body"),
                        None,
                    ),
                },
                30,
            )
            .expect_err("mismatched skill activation key must reject");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
    }

    #[test]
    fn skill_catalog_and_activation_context_are_planned_in_cache_preserving_order() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session(&mut drive);

        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let set_catalog = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY),
                    entry: skill_catalog_input(catalog_ref.clone()),
                },
                20,
            )
            .expect("set skill catalog context");
        commit_action(&mut drive, set_catalog);

        let skill_id = SkillId::new("skill-1");
        let activation_ref = BlobRef::from_bytes(b"skill body");
        let set_activations = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: skill_activation_context_key(&skill_id),
                    entry: skill_activation_input(skill_id.clone(), activation_ref.clone(), None),
                },
                21,
            )
            .expect("set skill activation context");
        commit_action(&mut drive, set_activations);

        let input_ref = BlobRef::from_bytes(b"input");
        request_run(&mut drive, input_ref.clone());

        let request = drive_until_generate(&mut drive);
        assert_eq!(request.session_id, session_id);
        let items = openai_items(&request);
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0].kind, ContextEntryKind::SkillCatalog));
        assert_eq!(items[0].content_ref, catalog_ref);
        assert!(matches!(
            &items[1].kind,
            ContextEntryKind::SkillActivation { skill_id: planned } if planned == &skill_id
        ));
        assert_eq!(items[1].content_ref, activation_ref);
        assert!(matches!(
            items[2].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(items[2].content_ref, input_ref);
    }

    #[test]
    fn run_scoped_skill_activation_context_expires_when_run_completes() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let skill_id = SkillId::new("skill-1");
        let set_activations = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: skill_activation_context_key(&skill_id),
                    entry: skill_activation_input(
                        skill_id,
                        BlobRef::from_bytes(b"skill body"),
                        Some(SKILL_ACTIVATION_PROVIDER_KIND_RUN),
                    ),
                },
                20,
            )
            .expect("set skill activation context");
        commit_action(&mut drive, set_activations);

        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let llm_request = drive_until_generate(&mut drive);
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: llm_request.run_id,
                    turn_id: llm_request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .expect("resume generation");
        commit_action(&mut drive, resumed);

        let complete_run = drive.next_action(31, 64).expect("complete run");
        commit_action(&mut drive, complete_run);

        assert!(
            drive
                .state()
                .context
                .entries
                .iter()
                .all(|item| !matches!(item.kind, ContextEntryKind::SkillActivation { .. }))
        );

        request_run(&mut drive, BlobRef::from_bytes(b"next input"));
        let next_request = drive_until_generate(&mut drive);
        let next_items = openai_items(&next_request);
        assert!(
            next_items
                .iter()
                .all(|item| !matches!(item.kind, ContextEntryKind::SkillActivation { .. }))
        );
    }

    #[test]
    fn drive_emits_llm_action_after_planned_generation_events_are_committed() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);
        let request = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    run_config(),
                ),
                20,
            )
            .expect("request run");
        commit_action(&mut drive, request);

        for observed_at_ms in 21..40 {
            let action = drive.next_action(observed_at_ms, 32).expect("next action");
            if let CoreAgentAction::GenerateLlm { request } = action {
                assert_eq!(request.session_id, session_id);
                return;
            }
            commit_action(&mut drive, action);
        }
        panic!("drive did not emit an LLM action");
    }

    #[test]
    fn drive_resumes_llm_result_into_append_action() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);
        let request = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    run_config(),
                ),
                20,
            )
            .expect("request run");
        commit_action(&mut drive, request);
        loop {
            let action = drive.next_action(21, 8).expect("next");
            if let CoreAgentAction::GenerateLlm { request } = action {
                let result = LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::Assistant,
                        },
                        content_ref: BlobRef::from_bytes(b"assistant output"),
                        media_type: None,
                        preview: None,
                        provider_kind: None,
                        provider_item_id: None,
                        token_estimate: None,
                    }],
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                    },
                };
                let resumed = drive
                    .resume_generation(result, 30)
                    .expect("resume generation");
                assert!(matches!(resumed, CoreAgentAction::AppendEvents { .. }));
                break;
            }
            commit_action(&mut drive, action);
        }
    }

    #[test]
    fn failed_generation_fails_run_without_starting_another_turn() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);
        let request = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    run_config(),
                ),
                20,
            )
            .expect("request run");
        commit_action(&mut drive, request);

        let llm_request = loop {
            let action = drive.next_action(21, 8).expect("next");
            if let CoreAgentAction::GenerateLlm { request } = action {
                break request;
            }
            commit_action(&mut drive, action);
        };
        let failure_ref = BlobRef::from_bytes(b"model failed");
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: llm_request.run_id,
                    turn_id: llm_request.turn_id,
                    status: LlmGenerationStatus::Failed,
                    failure_ref: Some(failure_ref.clone()),
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: None,
                        finish: LlmFinish::Failed,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .expect("resume failed generation");
        commit_action(&mut drive, resumed);

        let fail_run = drive.next_action(31, 8).expect("fail run");
        let entries = commit_action(&mut drive, fail_run);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Run(crate::RunEvent::Failed { .. })
        ));
        assert!(drive.state().runs.active.is_none());
        let completed = drive.state().runs.completed.last().expect("completed run");
        assert_eq!(completed.status, RunStatus::Failed);
        let failure = completed.failure.as_ref().expect("run failure");
        assert_eq!(failure.kind, RunFailureKind::ModelFailure);
        assert_eq!(failure.message_ref.as_ref(), Some(&failure_ref));

        assert!(matches!(
            drive.next_action(32, 8).expect("next"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn deferred_tool_batch_parks_and_next_action_does_not_reemit_invocation() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);

        let deferred = drive
            .resume_tool_batch_outcome(deferred_await_outcome(&request), 90)
            .expect("defer tool batch");
        let entries = commit_action(&mut drive, deferred);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Tool(ToolEvent::BatchDeferred { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(
            active_run
                .parked_await
                .as_ref()
                .expect("parked await")
                .batch_id,
            request.batch_id
        );
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert_eq!(batch.calls[0].status, ToolCallStatus::Pending);

        assert!(matches!(
            drive.next_action(91, 64).expect("next action"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn await_over_resolved_promise_parks_then_wakes_terminal() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let promise_id = crate::PromiseId::new("promise_done");
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Timer { fire_at_ms: 1 },
                scope: crate::PromiseScope::Run {
                    run_id: request.run_id,
                },
                status: crate::PromiseStatus::Resolved,
                payload_ref: Some(BlobRef::from_bytes(b"resolved output")),
                error_ref: None,
                deadline_ms: None,
            },
        );

        let deferred = drive
            .resume_tool_batch_outcome(
                deferred_await_outcome_with_spec(
                    &request,
                    AwaitSpec {
                        promise_ids: vec![promise_id],
                        mode: AwaitMode::All,
                        deadline_at_ms: None,
                        mailbox: false,
                    },
                ),
                90,
            )
            .expect("defer resolved await");
        commit_action(&mut drive, deferred);

        assert_eq!(await_wake(drive.state(), 91), Some(WakeReason::Terminal));
        let resumed = drive
            .admit_command(
                resume_await_command_with_claim(&request, WakeReason::Terminal),
                91,
            )
            .expect("resume terminal await");
        let entries = commit_action(&mut drive, resumed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Tool(ToolEvent::BatchResumed { .. })
        )));
        assert!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .expect("active run")
                .parked_await
                .is_none()
        );
    }

    #[test]
    fn zero_timeout_await_parks_then_wakes_timeout() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let promise_id = crate::PromiseId::new("promise_pending");
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Timer { fire_at_ms: 1_000 },
                scope: crate::PromiseScope::Run {
                    run_id: request.run_id,
                },
                status: crate::PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );

        let deferred = drive
            .resume_tool_batch_outcome(
                deferred_await_outcome_with_spec(
                    &request,
                    AwaitSpec {
                        promise_ids: vec![promise_id],
                        mode: AwaitMode::All,
                        deadline_at_ms: Some(90),
                        mailbox: false,
                    },
                ),
                90,
            )
            .expect("defer zero-timeout await");
        commit_action(&mut drive, deferred);

        assert_eq!(await_wake(drive.state(), 89), None);
        assert_eq!(await_wake(drive.state(), 90), Some(WakeReason::Timeout));
    }

    #[test]
    fn unknown_promise_await_fails_without_parking() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let deferred = drive
            .resume_tool_batch_outcome(
                deferred_await_outcome_with_spec(
                    &request,
                    AwaitSpec {
                        promise_ids: vec![crate::PromiseId::new("missing")],
                        mode: AwaitMode::All,
                        deadline_at_ms: None,
                        mailbox: false,
                    },
                ),
                90,
            )
            .expect("unknown promise await returns failed tool result");
        let entries = commit_action(&mut drive, deferred);

        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Tool(ToolEvent::CallCompleted { .. })
        )));
        assert!(!entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Tool(ToolEvent::BatchDeferred { .. })
        )));
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert!(active_run.parked_await.is_none());
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert_eq!(batch.calls[0].status, ToolCallStatus::Failed);
    }

    #[test]
    fn resume_tool_batch_command_clears_parked_batch_and_is_retry_safe() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let deferred = drive
            .resume_tool_batch_outcome(deferred_await_outcome(&request), 90)
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);

        let resumed = drive
            .admit_command(resume_await_command(&request), 91)
            .expect("resume command");
        let entries = commit_action(&mut drive, resumed);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Tool(ToolEvent::BatchResumed { .. })
        ));
        assert!(matches!(
            entries[1].event,
            CoreAgentEvent::Tool(ToolEvent::CallCompleted { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert!(active_run.parked_await.is_none());
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);

        let duplicate = drive
            .admit_command(resume_await_command(&request), 92)
            .expect("duplicate resume command");
        assert!(
            !matches!(duplicate, CoreAgentAction::AppendEvents { .. }),
            "duplicate resume must not append events: {duplicate:?}"
        );

        let completed = drive.next_action(93, 64).expect("complete batch");
        let entries = commit_action(&mut drive, completed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Tool(ToolEvent::BatchCompleted { .. })
        )));
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert!(active_run.tool_batches.get(&request.batch_id).is_none());
        assert!(
            active_run
                .completed_tool_batches
                .contains_key(&request.batch_id)
        );
    }

    #[test]
    fn inline_tool_batch_completion_is_unchanged() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);

        let completed = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::completed(completed_tool_result(&request)),
                90,
            )
            .expect("complete inline batch");
        let entries = commit_action(&mut drive, completed);
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Tool(ToolEvent::CallCompleted { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert!(active_run.parked_await.is_none());
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);

        let completed = drive.next_action(91, 64).expect("complete batch");
        let entries = commit_action(&mut drive, completed);
        assert!(entries.iter().all(|entry| {
            !matches!(
                entry.event,
                CoreAgentEvent::Tool(
                    ToolEvent::BatchDeferred { .. } | ToolEvent::BatchResumed { .. }
                )
            )
        }));
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Tool(ToolEvent::BatchCompleted { .. })
        )));
    }

    #[test]
    fn tool_batch_result_materializes_extra_model_visible_entries() {
        let session_id = SessionId::new("session-extra-tool-context");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let tool_result_ref = BlobRef::from_bytes(b"wait completed");
        let extra_ref = BlobRef::from_bytes(b"extra visible message");
        let mut result = completed_tool_result(&request);
        result.results[0]
            .model_visible_context_entries
            .push(message_input(ContextMessageRole::User, extra_ref.clone()));

        let completed = drive
            .resume_tool_batch_outcome(ToolBatchOutcome::completed(result), 90)
            .expect("complete inline batch");
        commit_action(&mut drive, completed);
        for observed_at_ms in 91..100 {
            if drive.state().context.entries.iter().any(|entry| {
                matches!(
                    entry.kind,
                    ContextEntryKind::Message {
                        role: ContextMessageRole::User
                    }
                ) && entry.content_ref == extra_ref
            }) {
                break;
            }
            let action = drive
                .next_action(observed_at_ms, 64)
                .expect("materialize result context");
            commit_action(&mut drive, action);
        }

        assert!(drive.state().context.entries.iter().any(|entry| {
            matches!(entry.kind, ContextEntryKind::ToolResult { .. })
                && entry.content_ref == tool_result_ref
        }));
        assert!(drive.state().context.entries.iter().any(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User
                }
            ) && entry.content_ref == extra_ref
        }));
    }

    fn request_run_with_submission(
        drive: &mut CoreAgentDrive,
        submission_id: &str,
        input_ref: BlobRef,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        drive.admit_command(
            request_run_command(
                Some(crate::SubmissionId::new(submission_id)),
                user_input(input_ref),
                run_config(),
            ),
            20,
        )
    }

    #[test]
    fn duplicate_submission_admits_as_no_op_while_queued() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);

        let accepted =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"x"))
                .expect("first request run");
        commit_action(&mut drive, accepted);
        assert_eq!(drive.state().runs.queued.len(), 1);

        let duplicate =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"x"))
                .expect("duplicate request run");
        assert!(
            !matches!(duplicate, CoreAgentAction::AppendEvents { .. }),
            "duplicate submission must not append events: {duplicate:?}"
        );
        assert_eq!(drive.state().runs.queued.len(), 1);
    }

    #[test]
    fn duplicate_submission_with_different_input_is_rejected() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);

        let accepted =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"x"))
                .expect("first request run");
        commit_action(&mut drive, accepted);

        let error =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"other"))
                .expect_err("duplicate with different input must fail");
        let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = error else {
            panic!("expected command rejection, got: {error:?}");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::DuplicateSubmission);
    }

    #[test]
    fn duplicate_submission_after_mailbox_consumption_admits_as_no_op() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"parked"));
        let _ = drive_until_generate(&mut drive);

        let submission_id = crate::SubmissionId::new("retry_mailbox");
        let source = RunRequestSource::Input {
            input: user_input(BlobRef::from_bytes(b"x")),
        };
        let message_id = crate::MessageId::new(1);
        let config_revision = drive.state().lifecycle.config_revision;
        commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Run(RunEvent::MessageBuffered {
                message_id,
                submission_id: Some(submission_id.clone()),
                submission_digest: crate::message_submission_digest(source.input()),
                input: source.input().to_vec(),
                run_config: run_config(),
                config_revision,
            }),
            30,
        )
        .expect("record buffered message submission");
        commit_core_event_result(
            &mut drive,
            CoreAgentEvent::Run(RunEvent::MessageConsumedByAwait {
                message_id,
                run_id: crate::RunId::new(1),
            }),
            30,
        )
        .expect("record consumed message submission");
        assert_eq!(drive.state().runs.messages.len(), 1);
        assert_eq!(
            drive.state().runs.messages[0].status,
            crate::MessageStatus::ConsumedByAwait
        );

        let mut replayed = CoreAgentDrive::from_replayed(
            SessionId::new("session-a"),
            drive.state().clone(),
            drive.head().cloned(),
        );
        let duplicate = replayed
            .admit_command(
                CoreAgentCommand::SubmitMessage(SubmitMessageCommand {
                    submission_id: Some(submission_id.clone()),
                    input: source.input().to_vec(),
                }),
                31,
            )
            .expect("duplicate consumed mailbox request");
        assert!(
            !matches!(duplicate, CoreAgentAction::AppendEvents { .. }),
            "duplicate consumed submission must not append events: {duplicate:?}"
        );

        let mismatch = replayed
            .admit_command(
                CoreAgentCommand::SubmitMessage(SubmitMessageCommand {
                    submission_id: Some(submission_id),
                    input: user_input(BlobRef::from_bytes(b"other")),
                }),
                32,
            )
            .expect_err("consumed duplicate with different input must fail");
        let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = mismatch else {
            panic!("expected command rejection, got: {mismatch:?}");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::DuplicateSubmission);
    }

    #[test]
    fn duplicate_submission_after_run_completion_admits_as_no_op() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);

        let accepted =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"x"))
                .expect("first request run");
        commit_action(&mut drive, accepted);
        let llm_request = drive_until_generate(&mut drive);
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: llm_request.run_id,
                    turn_id: llm_request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .expect("resume generation");
        commit_action(&mut drive, resumed);
        let complete_run = drive.next_action(31, 64).expect("complete run");
        commit_action(&mut drive, complete_run);
        let completed = drive.state().runs.completed.last().expect("completed run");
        assert_eq!(completed.status, RunStatus::Completed);
        assert!(completed.submission_digest.is_some());

        let duplicate =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"x"))
                .expect("duplicate after completion");
        assert!(
            !matches!(duplicate, CoreAgentAction::AppendEvents { .. }),
            "duplicate submission must not append events: {duplicate:?}"
        );
        assert_eq!(drive.state().runs.completed.len(), 1);
        assert!(drive.state().runs.queued.is_empty());

        let mismatch =
            request_run_with_submission(&mut drive, "retry_1", BlobRef::from_bytes(b"other"))
                .expect_err("completed duplicate with different input must fail");
        let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = mismatch else {
            panic!("expected command rejection, got: {mismatch:?}");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::DuplicateSubmission);
    }

    fn drain_to_idle(drive: &mut CoreAgentDrive, observed_at_ms: u64) {
        loop {
            let action = drive.next_action(observed_at_ms, 64).expect("next action");
            match action {
                CoreAgentAction::Idle | CoreAgentAction::Closed => return,
                CoreAgentAction::AppendEvents { .. } => {
                    commit_action(drive, action);
                }
                other => panic!("unexpected action while draining: {other:?}"),
            }
        }
    }

    /// Regression for the 2026-07-06 incident: a cancellation that lands
    /// while a tool batch is parked must still reach `cancelled` once the
    /// deferred batch resumes. Previously the tooling planner refused to
    /// complete batches for non-`active` runs while the run planner refused
    /// to cancel with an open batch — a planner deadlock.
    #[test]
    fn resume_of_deferred_batch_while_cancelling_reaches_cancelled() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let deferred = drive
            .resume_tool_batch_outcome(deferred_await_outcome(&request), 90)
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);

        let cancel = drive
            .admit_command(
                CoreAgentCommand::CancelRun {
                    run_id: request.run_id,
                },
                91,
            )
            .expect("request cancellation");
        commit_action(&mut drive, cancel);
        assert_eq!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .expect("active run")
                .status,
            RunStatus::Cancelling
        );

        let resumed = drive
            .admit_command(
                resume_await_command_with_claim(&request, WakeReason::Cancelled),
                92,
            )
            .expect("resume while cancelling");
        commit_action(&mut drive, resumed);

        let grace_request = drive_until_generate(&mut drive);
        assert_eq!(grace_request.run_id, request.run_id);
        assert_eq!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .expect("active run")
                .status,
            RunStatus::CancellingGrace
        );
        let grace_completed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: grace_request.run_id,
                    turn_id: grace_request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-grace".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                93,
            )
            .expect("complete grace turn");
        commit_action(&mut drive, grace_completed);

        drain_to_idle(&mut drive, 94);
        assert!(drive.state().runs.active.is_none());
        let completed = drive.state().runs.completed.last().expect("run record");
        assert_eq!(completed.status, RunStatus::Cancelled);
    }

    #[test]
    fn force_cancel_run_reaps_parked_cancelling_run_and_retries_are_noops() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let deferred = drive
            .resume_tool_batch_outcome(deferred_await_outcome(&request), 90)
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);
        let cancel = drive
            .admit_command(
                CoreAgentCommand::CancelRun {
                    run_id: request.run_id,
                },
                91,
            )
            .expect("request cancellation");
        commit_action(&mut drive, cancel);

        let run_id = drive.state().runs.active.as_ref().expect("active").run_id;
        let forced = drive
            .admit_command(CoreAgentCommand::ForceCancelRun { run_id }, 95)
            .expect("force cancel");
        let entries = commit_action(&mut drive, forced);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Run(crate::RunEvent::ForceCancelled { .. })
        ));
        assert!(drive.state().runs.active.is_none());
        assert_eq!(
            drive.state().runs.completed.last().expect("record").status,
            RunStatus::Cancelled
        );

        let retry = drive
            .admit_command(CoreAgentCommand::ForceCancelRun { run_id }, 96)
            .expect("force cancel retry");
        assert!(
            !matches!(retry, CoreAgentAction::AppendEvents { .. }),
            "force cancel retry must be a no-op: {retry:?}"
        );
    }

    #[test]
    fn force_close_cancels_active_and_queued_work_and_closes() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let deferred = drive
            .resume_tool_batch_outcome(deferred_await_outcome(&request), 90)
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);

        let queued = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"queued input")),
                    run_config(),
                ),
                91,
            )
            .expect("queue second run");
        commit_action(&mut drive, queued);
        assert_eq!(drive.state().runs.queued.len(), 1);

        let close = drive
            .admit_command(CoreAgentCommand::CloseSession { force: true }, 95)
            .expect("force close");
        let entries = commit_action(&mut drive, close);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Run(crate::RunEvent::ForceCancelled { .. })
        ));
        assert!(matches!(
            entries[1].event,
            CoreAgentEvent::Run(crate::RunEvent::QueuedCancelled { .. })
        ));
        assert!(matches!(
            entries[2].event,
            CoreAgentEvent::Lifecycle(crate::CoreAgentLifecycleEvent::Closed)
        ));
        assert_eq!(drive.state().lifecycle.status, CoreAgentStatus::Closed);
        assert!(drive.state().runs.active.is_none());
        assert!(drive.state().runs.queued.is_empty());
        assert_eq!(drive.state().runs.completed.len(), 2);
        assert!(
            drive
                .state()
                .runs
                .completed
                .iter()
                .all(|record| record.status == RunStatus::Cancelled)
        );

        let retry = drive
            .admit_command(CoreAgentCommand::CloseSession { force: true }, 96)
            .expect("force close retry");
        assert!(
            !matches!(retry, CoreAgentAction::AppendEvents { .. }),
            "force close retry must be a no-op: {retry:?}"
        );
    }

    #[test]
    fn close_without_force_still_rejects_active_work() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        drive_to_single_tool_invocation(&mut drive);
        let rejected = drive
            .admit_command(CoreAgentCommand::CloseSession { force: false }, 95)
            .expect_err("close with active work must be rejected");
        let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = rejected else {
            panic!("expected command rejection, got: {rejected:?}");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::ActiveWork);
    }

    fn promise_tool_result(
        request: &ToolInvocationBatchRequest,
        promise_id: &str,
    ) -> ToolInvocationBatchResult {
        let mut result = completed_tool_result(request);
        result.results[0].effects = vec![crate::promise_create_effect(
            &crate::PromiseId::new(promise_id),
            &crate::PromiseSource::Run {
                target_session_id: "child_session".to_owned(),
                target_run_id: 1,
            },
            None,
        )];
        result
    }

    #[test]
    fn workflow_port_effect_atomically_records_successful_call_and_emission() {
        let session_id = SessionId::new("session-port");
        let universe_id = uuid::Uuid::from_u128(7);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let definition = WorkflowToolPortDefinition {
            port_id: WorkflowToolPortId::new("report"),
            revision: 1,
            semantic_type: "lightspeed.work.report.v1".to_owned(),
            tool: test_tool_spec("work_report"),
        };
        let controller = WorkflowEndpointRef {
            workflow_id: "opaque work workflow id".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        };
        let declaration = crate::ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![crate::WorkflowToolPortDeclaration::new(
                definition.clone(),
                controller,
            )],
        );
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: universe_id,
                    workflow_ports: declaration,
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, open);
        install_test_tool(&mut drive, "work_report");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let request = drive_until_tool_batch_request(&mut drive, generation, "work_report");
        let binding = drive
            .state()
            .workflow_ports
            .bindings
            .get(&definition.port_id)
            .cloned()
            .expect("durable binding");
        let call = &request.calls[0];
        let invocation_id = crate::WorkflowToolInvocationId::for_call(
            universe_id,
            &session_id,
            request.run_id,
            request.turn_id,
            request.batch_id,
            &call.call_id,
            &binding.binding_fingerprint,
        );
        let invocation = WorkflowToolInvocation {
            invocation_id: invocation_id.clone(),
            port_id: definition.port_id,
            semantic_type: definition.semantic_type,
            schema_revision: definition.revision,
            binding_fingerprint: binding.binding_fingerprint,
            session_universe_id: universe_id,
            session_id,
            run_id: request.run_id,
            turn_id: request.turn_id,
            tool_batch_id: request.batch_id,
            tool_call_id: call.call_id.clone(),
            arguments_ref: call.arguments_ref.clone(),
            reply_promise_id: None,
        };
        let mut result = completed_tool_result(&request);
        result.results[0].effects = vec![crate::workflow_port_emit_effect(&invocation)];

        let resumed = drive
            .resume_tool_batch(result, 90)
            .expect("resume port tool");
        let CoreAgentAction::AppendEvents { events, .. } = &resumed else {
            panic!("expected append");
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.kind, "lightspeed.core.tool.call_completed");
        assert_eq!(
            events[1].event.kind,
            "lightspeed.core.workflow_port.emitted"
        );

        commit_action(&mut drive, resumed);
        assert_eq!(
            drive.state().workflow_ports.emissions.get(&invocation_id),
            Some(&invocation)
        );
    }

    #[test]
    fn tool_result_promise_effect_creates_pending_run_scoped_promise() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let run_id = request.run_id;
        let resumed = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::completed(promise_tool_result(&request, "promise_a")),
                90,
            )
            .expect("resume tool batch");
        let entries = commit_action(&mut drive, resumed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Promise(crate::PromiseEvent::Created { .. })
        )));

        let promise = drive
            .state()
            .promises
            .promises
            .get(&crate::PromiseId::new("promise_a"))
            .expect("promise in state");
        assert_eq!(promise.status, crate::PromiseStatus::Pending);
        assert_eq!(promise.scope, crate::PromiseScope::Run { run_id });
    }

    #[test]
    fn tool_result_detach_effect_promotes_promise_to_session_scope() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let run_id = request.run_id;
        let promise_id = crate::PromiseId::new("promise_a");
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Run {
                    target_session_id: "child_session".to_owned(),
                    target_run_id: 1,
                },
                scope: crate::PromiseScope::Run { run_id },
                status: crate::PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );

        let mut result = completed_tool_result(&request);
        result.results[0].effects = vec![crate::promise_detach_effect(&promise_id)];
        let resumed = drive
            .resume_tool_batch_outcome(ToolBatchOutcome::completed(result), 90)
            .expect("resume tool batch");
        let entries = commit_action(&mut drive, resumed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Promise(crate::PromiseEvent::Detached { .. })
        )));

        let promise = drive
            .state()
            .promises
            .promises
            .get(&promise_id)
            .expect("promise in state");
        assert_eq!(promise.status, crate::PromiseStatus::Pending);
        assert_eq!(promise.scope, crate::PromiseScope::Session);
    }

    #[test]
    fn run_terminal_cascade_skips_session_scoped_promises() {
        let mut state = CoreAgentState::new();
        let run_id = crate::RunId::new(1);
        let promise_id = crate::PromiseId::new("promise_a");
        state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Run {
                    target_session_id: "child_session".to_owned(),
                    target_run_id: 1,
                },
                scope: crate::PromiseScope::Session,
                status: crate::PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );
        let proposals = with_run_terminal_side_effects(
            &state,
            vec![CoreAgentEventProposal::new(
                CoreAgentJoins {
                    run_id: Some(run_id),
                    ..CoreAgentJoins::default()
                },
                CoreAgentEvent::Run(RunEvent::Completed {
                    run_id,
                    output_ref: None,
                }),
            )],
        );

        assert_eq!(proposals.len(), 1);
    }

    #[test]
    fn resolve_promise_is_first_writer_wins_and_rejects_unknown_ids() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let resumed = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::completed(promise_tool_result(&request, "promise_a")),
                90,
            )
            .expect("resume tool batch");
        commit_action(&mut drive, resumed);

        let payload_ref = BlobRef::from_bytes(b"child output");
        let resolve = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: crate::PromiseId::new("promise_a"),
                    resolution: crate::PromiseResolution::Resolved {
                        payload_ref: Some(payload_ref.clone()),
                    },
                },
                91,
            )
            .expect("resolve promise");
        commit_action(&mut drive, resolve);
        let promise = drive
            .state()
            .promises
            .promises
            .get(&crate::PromiseId::new("promise_a"))
            .expect("promise in state");
        assert_eq!(promise.status, crate::PromiseStatus::Resolved);
        assert_eq!(promise.payload_ref.as_ref(), Some(&payload_ref));

        // First writer wins: a late conflicting delivery is a no-op.
        let late = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: crate::PromiseId::new("promise_a"),
                    resolution: crate::PromiseResolution::Failed { error_ref: None },
                },
                92,
            )
            .expect("late delivery");
        assert!(
            !matches!(late, CoreAgentAction::AppendEvents { .. }),
            "late resolution must be a no-op: {late:?}"
        );
        assert_eq!(
            drive
                .state()
                .promises
                .promises
                .get(&crate::PromiseId::new("promise_a"))
                .expect("promise")
                .status,
            crate::PromiseStatus::Resolved
        );

        let unknown = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: crate::PromiseId::new("promise_missing"),
                    resolution: crate::PromiseResolution::Cancelled,
                },
                93,
            )
            .expect_err("unknown promise must be rejected");
        let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = unknown else {
            panic!("expected command rejection, got: {unknown:?}");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::UnknownReference);
    }
}
