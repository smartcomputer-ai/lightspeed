//! Substrate-neutral CoreAgent drive machine.
//!
//! The drive machine owns deterministic CoreAgent state and decides the next
//! action required to make progress. It does not perform async I/O, call
//! providers, invoke tools, or write storage. Local runtimes and workflow
//! substrates fulfill emitted actions and resume the drive with committed
//! entries or execution results.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AdmitCommand, ApplyEvent, BlobRef, CodecError, CommandError, ContextCompactionRequest,
    ContextCompactionResult, ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextEvent,
    ContextMessageRole, CoreAdmitCommand, CoreAgentCodec, CoreAgentEntry, CoreAgentEventKind,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus, CoreApplyEvent,
    CorePlanner, DomainError, LlmFinish, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, LlmRequest, PlanNext, PlanningError, SessionId, SessionPosition,
    ToolBatchId, ToolBatchOutcome, ToolBatchResumeDirective, ToolCallResult, ToolCallStatus,
    ToolEvent, ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationRequest,
    ToolInvocationResult, TurnEvent, TurnId, TurnOutcome,
    core::components::context::context_entries_from_inputs,
    session::{DynamicSessionEntry, DynamicUncommittedSessionEvent},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CoreAgentAction {
    AppendEvents {
        expected_head: Option<SessionPosition>,
        events: Vec<DynamicUncommittedSessionEvent>,
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
    codec: CoreAgentCodec,
    admit: CoreAdmitCommand,
    apply: CoreApplyEvent,
    planner: CorePlanner,
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
            codec: CoreAgentCodec,
            admit: CoreAdmitCommand,
            apply: CoreApplyEvent,
            planner: CorePlanner::core(),
            steps_taken: 0,
        }
    }

    pub fn admit_command(
        &mut self,
        command: crate::CoreAgentCommand,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = self.admit.admit(&self.state, command)?;
        self.append_action(proposals, observed_at_ms)
    }

    pub fn next_action(
        &mut self,
        observed_at_ms: u64,
        max_steps: usize,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = self.planner.plan_next(&self.state)?;
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
        entries: Vec<DynamicSessionEntry>,
    ) -> Result<Vec<CoreAgentEntry>, CoreAgentDriveError> {
        let decoded = entries
            .iter()
            .map(|entry| self.codec.decode_entry(entry))
            .collect::<Result<Vec<_>, _>>()?;
        for entry in &decoded {
            self.apply.apply(&mut self.state, entry)?;
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
        let proposals = tool_batch_result_proposals(&self.state, result)?;
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
                resume_directive,
            } => self.defer_tool_batch(batch_id, resume_directive, observed_at_ms),
        }
    }

    pub fn defer_tool_batch(
        &mut self,
        batch_id: ToolBatchId,
        resume_directive: ToolBatchResumeDirective,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = tool_batch_deferred_proposals(&self.state, batch_id, resume_directive)?;
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
        let events = proposals
            .into_iter()
            .map(|proposal| proposal.into_uncommitted(observed_at_ms))
            .map(|event| self.codec.encode_uncommitted(&event))
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
    let apply = CoreApplyEvent;
    for entry in entries {
        if let CoreAgentEventKind::Turn(TurnEvent::Planned {
            turn_id,
            run_id,
            request_fingerprint,
            config_revision,
            context_revision,
            toolset_revision,
        }) = &entry.event.kind
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
        apply.apply(&mut state, entry)?;
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
            CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
                base_revision: state.context.revision,
                entries: context_entries,
            }),
        ));
    }
    proposals.push(CoreAgentEventProposal::new(
        joins.clone(),
        CoreAgentEventKind::Turn(TurnEvent::GenerationCompleted {
            turn_id: result.turn_id,
            run_id: result.run_id,
            status: result.status,
            facts: result.facts,
        }),
    ));
    proposals.push(CoreAgentEventProposal::new(
        joins,
        CoreAgentEventKind::Turn(TurnEvent::Completed {
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
            CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
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
        CoreAgentEventKind::Context(ContextEvent::CompactionFinished {
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
    if batch.parked.is_some() {
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
    state: &CoreAgentState,
    batch_id: ToolBatchId,
    resume_directive: ToolBatchResumeDirective,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    resume_directive.validate()?;
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
    if batch.parked.is_some() {
        return Err(DomainError::InvariantViolation(format!(
            "tool batch {} is already deferred",
            batch_id
        )));
    }
    if !batch
        .calls
        .iter()
        .any(|call_state| call_state.status == ToolCallStatus::Pending)
    {
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
    let joins = CoreAgentJoins {
        run_id: Some(batch.run_id),
        turn_id: Some(batch.turn_id),
        tool_batch_id: Some(batch.batch_id),
        ..CoreAgentJoins::default()
    };
    Ok(vec![CoreAgentEventProposal::new(
        joins,
        CoreAgentEventKind::Tool(ToolEvent::BatchDeferred {
            run_id: batch.run_id,
            turn_id: batch.turn_id,
            batch_id: batch.batch_id,
            resume_directive,
        }),
    )])
}

pub fn tool_batch_result_proposals(
    state: &CoreAgentState,
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    validate_tool_batch_result(&result)?;
    validate_result_matches_active_tool_batch(state, &result, false)?;
    Ok(tool_call_completed_proposals(result))
}

pub fn resume_deferred_tool_batch_proposals(
    state: &CoreAgentState,
    batch_id: ToolBatchId,
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    if result.batch_id != batch_id {
        return Err(DomainError::InvariantViolation(format!(
            "resume command batch id {} does not match result batch id {}",
            batch_id, result.batch_id
        )));
    }
    validate_tool_batch_result(&result)?;
    if deferred_resume_is_duplicate(state, batch_id, &result)? {
        return Ok(Vec::new());
    }
    validate_result_matches_active_tool_batch(state, &result, true)?;
    let joins = CoreAgentJoins {
        run_id: Some(result.run_id),
        turn_id: Some(result.turn_id),
        tool_batch_id: Some(result.batch_id),
        ..CoreAgentJoins::default()
    };
    let mut proposals = vec![CoreAgentEventProposal::new(
        joins,
        CoreAgentEventKind::Tool(ToolEvent::BatchResumed {
            run_id: result.run_id,
            turn_id: result.turn_id,
            batch_id: result.batch_id,
        }),
    )];
    proposals.extend(tool_call_completed_proposals(result));
    Ok(proposals)
}

fn tool_call_completed_proposals(result: ToolInvocationBatchResult) -> Vec<CoreAgentEventProposal> {
    result
        .results
        .into_iter()
        .map(|result_item| {
            let call_id = result_item.call_id.clone();
            let joins = CoreAgentJoins {
                run_id: Some(result.run_id),
                turn_id: Some(result.turn_id),
                tool_batch_id: Some(result.batch_id),
                tool_call_id: Some(call_id),
                ..CoreAgentJoins::default()
            };
            CoreAgentEventProposal::new(
                joins,
                CoreAgentEventKind::Tool(ToolEvent::CallCompleted {
                    run_id: result.run_id,
                    turn_id: result.turn_id,
                    batch_id: result.batch_id,
                    result: invocation_result_to_call_result(result_item),
                }),
            )
        })
        .collect()
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
    match (require_parked, batch.parked.is_some()) {
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

fn deferred_resume_is_duplicate(
    state: &CoreAgentState,
    batch_id: ToolBatchId,
    result: &ToolInvocationBatchResult,
) -> Result<bool, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(false);
    };
    let requested_results = invocation_results_to_call_results(&result.results);
    if let Some(completed) = active_run.completed_tool_batches.get(&batch_id) {
        if completed.run_id != result.run_id || completed.turn_id != result.turn_id {
            return Err(DomainError::InvariantViolation(
                "duplicate resume result does not match completed tool batch".into(),
            ));
        }
        return Ok(completed.results == requested_results);
    }
    let Some(batch) = active_run.tool_batches.get(&batch_id) else {
        return Ok(false);
    };
    if batch.parked.is_some() {
        return Ok(false);
    }
    let actual_results = batch
        .calls
        .iter()
        .filter_map(|call_state| call_state.result.clone())
        .collect::<Vec<_>>();
    if actual_results.is_empty() {
        return Ok(false);
    }
    Ok(actual_results == requested_results)
}

fn invocation_results_to_call_results(results: &[ToolInvocationResult]) -> Vec<ToolCallResult> {
    results
        .iter()
        .cloned()
        .map(invocation_result_to_call_result)
        .collect()
}

fn invocation_result_to_call_result(result: ToolInvocationResult) -> ToolCallResult {
    ToolCallResult {
        call_id: result.call_id,
        status: result.status,
        output_ref: result.output_ref,
        model_visible_output_ref: result.model_visible_output_ref,
        error_ref: result.error_ref,
        effects: result.effects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlobRef, CommandRejectionKind, CompactionPolicy, ContextCompactionStatus,
        ContextCompactionTrigger, ContextConfig, ContextConfigPatch, ContextEntry, ContextEntryId,
        ContextEntryInput, ContextEntryKey, ContextEntryKind, ContextRemovalReason,
        ContextRewriteReason, CoreAgentCommand, FunctionToolSpec, LlmGenerationFacts,
        ModelSelection, OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND, ObservedToolCall,
        OptionalConfigPatch, ProviderApiKind, RunConfig, RunFailureKind, RunStatus,
        SKILL_ACTIVATION_PROVIDER_KIND_RUN, SKILL_CATALOG_CONTEXT_KEY, SessionConfig,
        SessionConfigPatch, SkillId, TokenEstimate, TokenEstimateQuality, ToolBatchOutcome,
        ToolBatchResumeDirective, ToolChoice, ToolChoiceMode, ToolEffect, ToolInvocationResult,
        ToolKind, ToolName, ToolParallelism, ToolSpec, ToolTargetRequirement, TurnConfig,
        TurnConfigPatch, TurnStatus, skill_activation_context_key,
    };

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            run: RunConfig {
                max_turns: None,
                max_tool_rounds: None,
                model_override: None,
                max_output_tokens: None,
                provider_params: None,
                tool_choice: None,
            },
            turn: TurnConfig {
                max_output_tokens: None,
                tool_choice: None,
                provider_params: None,
            },
            context: ContextConfig { compaction: None },
            tools: Default::default(),
        }
    }

    fn run_config() -> RunConfig {
        RunConfig {
            max_turns: None,
            max_tool_rounds: None,
            model_override: None,
            max_output_tokens: None,
            provider_params: None,
            tool_choice: None,
        }
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
                DynamicSessionEntry {
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
        kind: CoreAgentEventKind,
        observed_at_ms: u64,
    ) -> Result<Vec<CoreAgentEntry>, CoreAgentDriveError> {
        let proposal = CoreAgentEventProposal::new(CoreAgentJoins::default(), kind);
        let uncommitted = proposal.into_uncommitted(observed_at_ms);
        let event = drive.codec.encode_uncommitted(&uncommitted)?;
        let seq = drive.head().map_or(1, |position| position.seq.as_u64() + 1);
        let entry = DynamicSessionEntry {
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(input_ref),
                    run_config: run_config(),
                },
                20,
            )
            .expect("request run");
        commit_action(drive, request);
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
        install_test_tool(drive, "agent_wait");
        request_run(drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(drive);
        drive_until_tool_batch_request(drive, request, "agent_wait")
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
                model_visible_output_ref: Some(BlobRef::from_bytes(b"wait completed")),
                error_ref: None,
                effects: vec![ToolEffect {
                    kind: "test".to_owned(),
                    data: Default::default(),
                }],
            }],
        }
    }

    fn wait_resume_directive() -> ToolBatchResumeDirective {
        ToolBatchResumeDirective::new(
            "fleet.agent_wait",
            serde_json::json!({
                "waits": [
                    {
                        "target_session_id": "child",
                        "run_id": 1
                    }
                ],
                "mode": "all"
            }),
        )
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
                if let CoreAgentEventKind::Turn(event @ TurnEvent::Planned { .. }) =
                    entry.event.kind
                {
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: vec![message_input(
                        ContextMessageRole::Assistant,
                        BlobRef::from_bytes(b"assistant"),
                    )],
                    run_config: run_config(),
                },
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: vec![provider_opaque_input(BlobRef::from_bytes(b"native"))],
                    run_config: run_config(),
                },
                20,
            )
            .expect("provider-opaque run input");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().runs.queued.len(), 1);
        assert!(matches!(
            drive.state().runs.queued[0].input[0].kind,
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
    fn upsert_context_accepts_instruction_entry_with_instruction_key() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        let key = ContextEntryKey::new("instructions.100.base");

        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
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
        assert!(active_run.input.entry_ids.is_empty());
        assert!(drive.state().context.entries.is_empty());

        let materialize_input = drive.next_action(22, 64).expect("materialize input");
        let entries = commit_action(&mut drive, materialize_input);
        let CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
            entries: applied, ..
        }) = &entries[0].event.kind
        else {
            panic!("expected context entries");
        };
        assert_eq!(applied.len(), 1);
        assert!(matches!(
            applied[0].source,
            ContextEntrySource::RunInput { input_index: 0, .. }
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(active_run.input.entry_ids, vec![applied[0].entry_id]);
        assert_eq!(active_run.input.consumed_by_turn_id, None);
        assert_eq!(drive.state().context.entries.len(), 1);

        let start_turn = drive.next_action(23, 64).expect("start turn");
        let entries = commit_action(&mut drive, start_turn);
        assert!(matches!(
            entries[0].event.kind,
            CoreAgentEventKind::Turn(TurnEvent::Started { .. })
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
            .input
            .entry_ids[0];
        let base_revision = drive.state().context.revision;
        let error = commit_core_event_result(
            &mut drive,
            CoreAgentEventKind::Context(ContextEvent::EntriesRemoved {
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
        let entry_id = active_run.input.entry_ids[0];
        assert_eq!(active_run.input.consumed_by_turn_id, Some(request.turn_id));

        let base_revision = drive.state().context.revision;
        commit_core_event_result(
            &mut drive,
            CoreAgentEventKind::Context(ContextEvent::EntriesRemoved {
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
            .input
            .entry_ids[0];

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
        let CoreAgentEventKind::Context(ContextEvent::EntriesRemoved {
            entry_ids, reason, ..
        }) = &entries[0].event.kind
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
        let CoreAgentEventKind::Context(ContextEvent::CompactionRequested { trigger, .. }) =
            &requested_entries[0].event.kind
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
            completed_entries[0].event.kind,
            CoreAgentEventKind::Context(ContextEvent::EntriesApplied { .. })
        ));
        assert!(matches!(
            completed_entries[1].event.kind,
            CoreAgentEventKind::Context(ContextEvent::CompactionFinished {
                status: ContextCompactionStatus::Succeeded,
                ..
            })
        ));
        assert!(!drive.state().context.pending_compaction);

        let prune = drive.next_action(33, 64).expect("prune compacted entries");
        let pruned_entries = commit_action(&mut drive, prune);
        let CoreAgentEventKind::Context(ContextEvent::EntriesRemoved {
            entry_ids, reason, ..
        }) = &pruned_entries[0].event.kind
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

        let CoreAgentEventKind::Context(ContextEvent::CompactionFinished {
            status,
            failure_ref: event_failure_ref,
            ..
        }) = &completed_entries[0].event.kind
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"new work")),
                    run_config: run_config(),
                },
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
        let CoreAgentEventKind::Context(ContextEvent::CompactionRequested { trigger, .. }) =
            &requested_entries[0].event.kind
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
            requested_entries[0].event.kind,
            CoreAgentEventKind::Context(ContextEvent::CompactionRequested {
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
            CoreAgentEventKind::Context(ContextEvent::EntriesRemoved {
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
            CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
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
            CoreAgentEventKind::Context(ContextEvent::StateReplaced {
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
        let CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
            entries: applied, ..
        }) = &entries[0].event.kind
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
    fn patch_session_config_updates_full_config_snapshot() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let patch = SessionConfigPatch {
            turn: TurnConfigPatch {
                max_output_tokens: Some(OptionalConfigPatch::Set(2048)),
                ..TurnConfigPatch::default()
            },
            context: ContextConfigPatch::default(),
            ..SessionConfigPatch::default()
        };
        let action = drive
            .admit_command(
                CoreAgentCommand::PatchSessionConfig {
                    expected_revision: Some(0),
                    patch,
                },
                20,
            )
            .expect("patch config");
        commit_action(&mut drive, action);

        let config = drive
            .state()
            .lifecycle
            .config
            .as_ref()
            .expect("session config");
        assert_eq!(drive.state().lifecycle.config_revision, 1);
        assert_eq!(config.turn.max_output_tokens, Some(2048));
    }

    #[test]
    fn patch_session_config_rejects_specific_tool_choice_for_missing_tool() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let patch = SessionConfigPatch {
            turn: TurnConfigPatch {
                tool_choice: Some(OptionalConfigPatch::Set(ToolChoice {
                    mode: ToolChoiceMode::Specific {
                        tool_name: ToolName::new("missing_tool"),
                    },
                    disable_parallel_tool_use: None,
                })),
                ..TurnConfigPatch::default()
            },
            ..SessionConfigPatch::default()
        };

        let error = drive
            .admit_command(
                CoreAgentCommand::PatchSessionConfig {
                    expected_revision: Some(0),
                    patch,
                },
                20,
            )
            .expect_err("patch must reject missing specific tool choice");

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
        run_config.tool_choice = Some(ToolChoice {
            mode: ToolChoiceMode::Specific {
                tool_name: ToolName::new("missing_tool"),
            },
            disable_parallel_tool_use: None,
        });

        let error = drive
            .admit_command(
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config,
                },
                20,
            )
            .expect_err("run must reject missing specific tool choice");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::InvariantViolation);
    }

    #[test]
    fn patch_session_config_rejects_queued_work() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);
        let request = drive
            .admit_command(
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
                20,
            )
            .expect("request run");
        commit_action(&mut drive, request);

        let error = drive
            .admit_command(
                CoreAgentCommand::PatchSessionConfig {
                    expected_revision: Some(0),
                    patch: SessionConfigPatch::default(),
                },
                30,
            )
            .expect_err("patch must reject queued work");

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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
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
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input: user_input(BlobRef::from_bytes(b"input")),
                    run_config: run_config(),
                },
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
            entries[0].event.kind,
            CoreAgentEventKind::Run(crate::RunEvent::Failed { .. })
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
            .resume_tool_batch_outcome(
                ToolBatchOutcome::Deferred {
                    batch_id: request.batch_id,
                    resume_directive: wait_resume_directive(),
                },
                90,
            )
            .expect("defer tool batch");
        let entries = commit_action(&mut drive, deferred);
        assert!(matches!(
            entries[0].event.kind,
            CoreAgentEventKind::Tool(ToolEvent::BatchDeferred { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert!(batch.parked.is_some());
        assert_eq!(batch.calls[0].status, ToolCallStatus::Pending);

        assert!(matches!(
            drive.next_action(91, 64).expect("next action"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn resume_tool_batch_command_clears_parked_batch_and_is_retry_safe() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let deferred = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::Deferred {
                    batch_id: request.batch_id,
                    resume_directive: wait_resume_directive(),
                },
                90,
            )
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);

        let result = completed_tool_result(&request);
        let resumed = drive
            .admit_command(
                CoreAgentCommand::ResumeToolBatch {
                    batch_id: request.batch_id,
                    result: result.clone(),
                },
                91,
            )
            .expect("resume command");
        let entries = commit_action(&mut drive, resumed);
        assert!(matches!(
            entries[0].event.kind,
            CoreAgentEventKind::Tool(ToolEvent::BatchResumed { .. })
        ));
        assert!(matches!(
            entries[1].event.kind,
            CoreAgentEventKind::Tool(ToolEvent::CallCompleted { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert!(batch.parked.is_none());
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);

        let duplicate = drive
            .admit_command(
                CoreAgentCommand::ResumeToolBatch {
                    batch_id: request.batch_id,
                    result,
                },
                92,
            )
            .expect("duplicate resume command");
        assert!(
            !matches!(duplicate, CoreAgentAction::AppendEvents { .. }),
            "duplicate resume must not append events: {duplicate:?}"
        );

        let completed = drive.next_action(93, 64).expect("complete batch");
        let entries = commit_action(&mut drive, completed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event.kind,
            CoreAgentEventKind::Tool(ToolEvent::BatchCompleted { .. })
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
            entries[0].event.kind,
            CoreAgentEventKind::Tool(ToolEvent::CallCompleted { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        assert!(batch.parked.is_none());
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);

        let completed = drive.next_action(91, 64).expect("complete batch");
        let entries = commit_action(&mut drive, completed);
        assert!(entries.iter().all(|entry| {
            !matches!(
                entry.event.kind,
                CoreAgentEventKind::Tool(
                    ToolEvent::BatchDeferred { .. } | ToolEvent::BatchResumed { .. }
                )
            )
        }));
        assert!(entries.iter().any(|entry| matches!(
            entry.event.kind,
            CoreAgentEventKind::Tool(ToolEvent::BatchCompleted { .. })
        )));
    }

    fn request_run_with_submission(
        drive: &mut CoreAgentDrive,
        submission_id: &str,
        input_ref: BlobRef,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        drive.admit_command(
            CoreAgentCommand::RequestRun {
                submission_id: Some(crate::SubmissionId::new(submission_id)),
                input: user_input(input_ref),
                run_config: run_config(),
            },
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
}
