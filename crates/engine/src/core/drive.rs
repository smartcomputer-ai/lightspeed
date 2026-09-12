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

pub const AWAIT_TOOL_ID: &str = "concurrency.await";

use crate::{
    ApprovalEvent, ApprovalId, ApprovalRequested, AwaitMode, AwaitSpec, BlobRef, CodecError,
    CommandError, ContextCompactionRequest, ContextCompactionResult, ContextEntryInput,
    ContextEntryKind, ContextEntrySource, ContextEvent, ContextMessageRole, CoreAgentCodec,
    CoreAgentEntry, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, LlmFinish, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, LlmRequest, NativeMcpApprovalRequest, PlanningError, PromiseEvent,
    PromiseId, PromiseOwnership, PromiseStatus, ResumeToolBatchCommand, RunEvent, SessionId,
    SessionPosition, ToolBatchId, ToolBatchOutcome, ToolBatchResumeOutput, ToolBatchSuspension,
    ToolCallId, ToolCallResult, ToolCallStatus, ToolEvent, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationRequest, ToolInvocationResult, TurnEvent, TurnId,
    TurnOutcome, WakeReason,
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
        self.next_action_with_limit(observed_at_ms, Some(max_steps))
    }

    /// Plan the next action without an orchestration-step ceiling.
    ///
    /// Hosted runtimes use this entry point because model, tool, compaction,
    /// and append transitions are ordinary progress. Bounded runners can keep
    /// using [`Self::next_action`] when a step budget is an explicit product
    /// or test policy.
    pub fn next_action_unbounded(
        &mut self,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        self.next_action_with_limit(observed_at_ms, None)
    }

    fn next_action_with_limit(
        &mut self,
        observed_at_ms: u64,
        max_steps: Option<usize>,
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

    /// Accept one terminal call result of the active tool batch.
    ///
    /// Per-call completion is the engine contract for progressive batches:
    /// each terminal result appends durably on its own, a failed call never
    /// re-runs a completed sibling, and the batch completes when its last
    /// call turns terminal.
    pub fn resume_tool_call(
        &mut self,
        batch_id: ToolBatchId,
        result: ToolInvocationResult,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let active_run = self.state.runs.active.as_ref().ok_or_else(|| {
            DomainError::InvariantViolation("tool call result requires an active run".into())
        })?;
        if active_run.active_tool_batch_id != Some(batch_id) {
            return Err(DomainError::InvariantViolation(
                "tool call result does not match active tool batch".into(),
            )
            .into());
        }
        let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
        })?;
        let result = ToolInvocationBatchResult {
            run_id: batch.run_id,
            turn_id: batch.turn_id,
            batch_id: batch.batch_id,
            results: vec![result],
        };
        self.resume_tool_batch(result, observed_at_ms)
    }

    /// Park the run on one or more native MCP calls of the active batch that
    /// need a run-owned decision. All requests and the single park append as
    /// one action: a run parks once per dispatch, however many calls are
    /// gated, and unparks only when every decision has landed.
    pub fn request_native_mcp_approvals(
        &mut self,
        batch_id: ToolBatchId,
        approvals: Vec<NativeMcpApprovalRequest>,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, CoreAgentDriveError> {
        let proposals = self.native_mcp_approval_proposals(batch_id, approvals)?;
        self.append_action(proposals, observed_at_ms)
    }

    fn native_mcp_approval_proposals(
        &self,
        batch_id: ToolBatchId,
        approvals: Vec<NativeMcpApprovalRequest>,
    ) -> Result<Vec<CoreAgentEventProposal>, CoreAgentDriveError> {
        if approvals.is_empty() {
            return Err(DomainError::InvariantViolation(
                "native MCP approval park requires at least one gated call".into(),
            )
            .into());
        }
        let active_run = self.state.runs.active.as_ref().ok_or_else(|| {
            DomainError::InvariantViolation("native MCP approval requires an active run".into())
        })?;
        let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {batch_id} is missing"))
        })?;
        if active_run.active_tool_batch_id != Some(batch_id) {
            return Err(DomainError::InvariantViolation(
                "native MCP approval does not match the active tool batch".into(),
            )
            .into());
        }
        let mut seen = BTreeSet::new();
        for approval in &approvals {
            let pending = batch.calls.iter().any(|call| {
                call.call.call_id == approval.call_id
                    && call.status == crate::ToolCallStatus::Pending
            });
            if !pending || !seen.insert(approval.call_id.clone()) {
                return Err(DomainError::InvariantViolation(
                    "native MCP approval does not match a pending active call".into(),
                )
                .into());
            }
        }
        let mut next = self.state.id_cursors.last_approval_id;
        let mut proposals = Vec::with_capacity(approvals.len() + 1);
        for NativeMcpApprovalRequest { call_id, subject } in approvals {
            next = next.checked_add(1).ok_or_else(|| {
                DomainError::InvariantViolation("approval id cursor exhausted".into())
            })?;
            let approval_id = crate::ApprovalId::new(format!("approval_{next}"));
            proposals.push(CoreAgentEventProposal::new(
                CoreAgentJoins {
                    run_id: Some(batch.run_id),
                    turn_id: Some(batch.turn_id),
                    tool_batch_id: Some(batch.batch_id),
                    tool_call_id: Some(call_id.clone()),
                    ..CoreAgentJoins::default()
                },
                CoreAgentEvent::Approval(crate::ApprovalEvent::Requested {
                    approval: crate::ApprovalRequested {
                        approval_id,
                        run_id: batch.run_id,
                        subject,
                        continuation: crate::ApprovalContinuation::NativeMcp { call_id },
                    },
                }),
            ));
        }
        proposals.push(CoreAgentEventProposal::new(
            CoreAgentJoins {
                run_id: Some(batch.run_id),
                turn_id: Some(batch.turn_id),
                tool_batch_id: Some(batch.batch_id),
                ..CoreAgentJoins::default()
            },
            CoreAgentEvent::Approval(crate::ApprovalEvent::RunParked {
                run_id: batch.run_id,
            }),
        ));
        Ok(proposals)
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
            ToolBatchOutcome::AwaitingApproval {
                batch_id,
                completed_results,
                approvals,
            } => {
                // Ungated siblings complete durably in the same append that
                // parks the run, so a re-dispatch after the decisions only
                // ever carries the calls that are still pending.
                let mut proposals = Vec::new();
                if !completed_results.is_empty() {
                    let active_run = self.state.runs.active.as_ref().ok_or_else(|| {
                        DomainError::InvariantViolation(
                            "tool batch result requires an active run".into(),
                        )
                    })?;
                    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
                        DomainError::InvariantViolation(format!("tool batch {batch_id} is missing"))
                    })?;
                    proposals.extend(tool_batch_result_proposals_for_session(
                        &self.session_id,
                        &self.state,
                        ToolInvocationBatchResult {
                            run_id: batch.run_id,
                            turn_id: batch.turn_id,
                            batch_id: batch.batch_id,
                            results: completed_results,
                        },
                    )?);
                }
                proposals.extend(self.native_mcp_approval_proposals(batch_id, approvals)?);
                self.append_action(proposals, observed_at_ms)
            }
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

    fn increment_steps(&mut self, max_steps: Option<usize>) -> bool {
        if max_steps.is_some_and(|max_steps| self.steps_taken >= max_steps) {
            return false;
        }
        self.steps_taken = self.steps_taken.saturating_add(1);
        true
    }
}

fn with_run_terminal_side_effects(
    state: &CoreAgentState,
    proposals: Vec<CoreAgentEventProposal>,
) -> Vec<CoreAgentEventProposal> {
    let mut output = Vec::with_capacity(proposals.len());
    let mut cancelled = BTreeSet::<PromiseId>::new();
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
    // A cancelling run never asks the runtime for work; the turn planner
    // cancels its open turn instead.
    if active_run.status != crate::RunStatus::Active {
        return Ok(None);
    }
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
    let approval_requests = result.facts.approval_requests.clone();
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
    let mut next_approval_id = state.id_cursors.last_approval_id;
    for observed in &approval_requests {
        next_approval_id = next_approval_id.checked_add(1).ok_or_else(|| {
            DomainError::InvariantViolation("approval id cursor exhausted".to_owned())
        })?;
        proposals.push(CoreAgentEventProposal::new(
            joins.clone(),
            CoreAgentEvent::Approval(ApprovalEvent::Requested {
                approval: ApprovalRequested {
                    approval_id: ApprovalId::new(format!("approval_{next_approval_id}")),
                    run_id: result.run_id,
                    subject: observed.subject.clone(),
                    continuation: observed.continuation.clone(),
                },
            }),
        ));
    }
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
            // A content filter (a provider refusal) and an output-cap cut-off
            // are terminal for the turn: the provider did not finish serving
            // the request, so the run fails with the adapter's reason instead
            // of completing as an empty or partial answer. The result's
            // context entries (a truncated turn's partial text) are still
            // applied above, so the user sees what was produced.
            LlmFinish::Failed | LlmFinish::ContentFilter | LlmFinish::Length => {
                TurnOutcome::Failed {
                    failure_ref: result.failure_ref.clone(),
                }
            }
            LlmFinish::Stop | LlmFinish::Unknown if !result.facts.approval_requests.is_empty() => {
                TurnOutcome::ApprovalsRequested
            }
            LlmFinish::Stop | LlmFinish::Unknown => TurnOutcome::FinalOutput {
                output: final_output(&result.context_entries),
            },
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

fn final_output(context_entries: &[ContextEntryInput]) -> Option<crate::ContentRef> {
    context_entries
        .iter()
        .rev()
        .find_map(|entry| match entry.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(entry.content.clone()),
            _ => None,
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
    // A cancelling run never asks the runtime for work; the tooling planner
    // cancels its pending calls instead.
    if active_run.status != crate::RunStatus::Active {
        return Ok(None);
    }
    let Some(batch_id) = active_run.active_tool_batch_id else {
        return Ok(None);
    };
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active tool batch {} is missing", batch_id))
    })?;
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Ok(None);
    }
    let calls = batch
        .calls
        .iter()
        .filter(|call_state| call_state.status == ToolCallStatus::Pending)
        .map(|call_state| {
            let workflow_tool = call_state
                .call
                .tool_id
                .as_ref()
                .and_then(|id| state.workflow_tools.binding_for_tool_name(id))
                .map(|binding| {
                    crate::WorkflowToolCallRuntime::v1(
                        binding.clone(),
                        state
                            .workflow_tools
                            .emission_count(batch.run_id, &binding.definition.tool_id),
                    )
                });
            // Native MCP routing is decided here, once per dispatch, so the
            // batch-unit and per-call execution paths see identical facts
            // (including the run-owned approval decision, if any).
            let remote_mcp = crate::remote_mcp_call_runtime(state, &call_state.call);
            ToolInvocationRequest {
                builtin: call_state
                    .call
                    .tool_id
                    .as_ref()
                    .and_then(|id| state.tooling.tools.get(id))
                    .and_then(|tool| match &tool.kind {
                        crate::ToolKind::Builtin(spec) => Some(crate::BuiltinToolCallRuntime {
                            spec: spec.clone(),
                            model: active_run
                                .run_config
                                .model_override
                                .clone()
                                .or_else(|| {
                                    state
                                        .lifecycle
                                        .config
                                        .as_ref()
                                        .map(|config| config.model.clone())
                                })
                                .expect("active run has a configured model"),
                        }),
                        _ => None,
                    }),
                call_id: call_state.call.call_id.clone(),
                tool_id: call_state.call.tool_id.clone(),
                tool_name: call_state.call.tool_name.clone(),
                arguments_ref: call_state.call.arguments_ref.clone(),
                workflow_tool,
                promise_control: None,
                remote_mcp,
            }
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
        promise_id_base: batch.promise_id_base,
        vfs_working_directory: state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .and_then(|vfs| vfs.working_directory.clone()),
        workspace_links: state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.vfs.as_ref())
            .map(|vfs| vfs.workspace_links.clone())
            .unwrap_or_default(),
        active_environment_id: state.environment.active_environment_id.clone(),
        environment_policy: state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.environments.as_ref())
            .map(crate::EnvironmentPolicyRuntime::from_feature),
        subagents_policy: state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.subagents.clone()),
        calls,
    }))
}

pub fn attach_promise_control_runtime(
    state: &CoreAgentState,
    mut request: ToolInvocationBatchRequest,
    facts: crate::PromiseControlArgumentFacts,
) -> Result<ToolInvocationBatchRequest, DomainError> {
    let expected = request.promise_control_argument_request().ok_or_else(|| {
        DomainError::InvariantViolation(
            "promise-control argument facts supplied for a batch without control calls".to_owned(),
        )
    })?;
    if facts.version != crate::PromiseControlArgumentFacts::VERSION {
        return Err(DomainError::InvariantViolation(format!(
            "unsupported promise-control argument facts version {}",
            facts.version
        )));
    }
    if facts.calls.len() != expected.calls.len() {
        return Err(DomainError::InvariantViolation(format!(
            "promise-control argument facts contain {} calls, expected {}",
            facts.calls.len(),
            expected.calls.len()
        )));
    }

    for (expected_call, call_facts) in expected.calls.iter().zip(&facts.calls) {
        if call_facts.call_id() != &expected_call.call_id {
            return Err(DomainError::InvariantViolation(format!(
                "promise-control argument facts call {} does not match expected call {}",
                call_facts.call_id(),
                expected_call.call_id
            )));
        }
        let call = request
            .calls
            .iter_mut()
            .find(|call| call.call_id == expected_call.call_id)
            .expect("expected promise-control call came from this request");
        if call.promise_control.is_some() {
            return Err(DomainError::InvariantViolation(format!(
                "promise-control runtime facts already attached to call {}",
                call.call_id
            )));
        }
        let crate::PromiseControlArgumentCallFacts::Parsed { promise_ids, .. } = call_facts else {
            continue;
        };
        if promise_ids.is_empty() || promise_ids.len() > 32 {
            return Err(DomainError::InvariantViolation(format!(
                "promise-control call {} has an invalid bounded id count {}",
                call.call_id,
                promise_ids.len()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut controls = Vec::with_capacity(promise_ids.len());
        for promise_id in promise_ids {
            if !seen.insert(promise_id) {
                return Err(DomainError::InvariantViolation(format!(
                    "promise-control call {} contains duplicate promise {}",
                    call.call_id, promise_id
                )));
            }
            let projected = match state.promises.promises.get(promise_id) {
                Some(promise) => crate::PromiseControlStateRuntime::Known {
                    ownership: promise.ownership,
                    scope: promise.scope.clone(),
                    promise_status: promise.status,
                },
                None => crate::PromiseControlStateRuntime::Unknown,
            };
            controls.push(crate::PromiseControlRuntime {
                promise_id: promise_id.clone(),
                state: projected,
            });
        }
        call.promise_control = Some(crate::PromiseControlCallRuntime::v1(controls));
    }
    Ok(request)
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
    if active_run.parked_tool_batch.is_some() {
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
                && call_state
                    .call
                    .tool_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == AWAIT_TOOL_ID)
        })
        .map(|call_state| call_state.call.call_id.clone())
        .collect::<Vec<_>>();
    let mut joined_call_ids = BTreeSet::new();
    for result_item in &completed_result.results {
        for effect in &result_item.effects {
            let Some(invocation) =
                crate::core::components::workflow_tool::invocation_from_emit_effect(effect)?
            else {
                continue;
            };
            if state
                .workflow_tools
                .bindings
                .get(&invocation.tool_id)
                .is_some_and(|binding| {
                    matches!(
                        binding.completion,
                        crate::WorkflowToolCompletion::Joined { .. }
                    )
                })
            {
                joined_call_ids.insert(result_item.call_id.clone());
            }
        }
    }
    if !joined_call_ids.is_empty() && !pending_await_call_ids.is_empty() {
        let mut results = completed_result
            .results
            .into_iter()
            .map(|result| {
                if joined_call_ids.contains(&result.call_id) {
                    invalid_await_tool_result(
                        result.call_id,
                        "Joined workflow calls cannot share a batch with await".to_owned(),
                    )
                } else {
                    result
                }
            })
            .collect::<Vec<_>>();
        for await_call_id in pending_await_call_ids {
            results.push(invalid_await_tool_result(
                await_call_id,
                "await cannot share a batch with Joined workflow calls".to_owned(),
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
        call_state.call.call_id == call_id
            && call_state
                .call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == AWAIT_TOOL_ID)
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
            suspension: ToolBatchSuspension::AwaitTool { call_id, spec },
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

pub fn resume_tool_batch_proposals(
    state: &CoreAgentState,
    command: ResumeToolBatchCommand,
    observed_at_ms: u64,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    if active_run.run_id != command.run_id {
        return Ok(Vec::new());
    }
    let Some(parked) = active_run.parked_tool_batch.as_ref() else {
        return Ok(Vec::new());
    };
    if parked.batch_id != command.batch_id {
        return Ok(Vec::new());
    }
    if command.claim_observed_at_ms > observed_at_ms {
        return Err(DomainError::InvariantViolation(
            "tool batch resume claim is observed in the future".to_owned(),
        ));
    }
    let Some(actual) = await_wake(state, command.claim_observed_at_ms) else {
        return Err(DomainError::InvariantViolation(
            "tool batch resume claim has no satisfied wake".to_owned(),
        ));
    };
    if actual != command.claim {
        return Err(DomainError::InvariantViolation(format!(
            "tool batch resume claim {:?} does not match current wake {:?}",
            command.claim, actual
        )));
    }
    let result = match (&parked.suspension, command.output) {
        (
            ToolBatchSuspension::AwaitTool { .. },
            ToolBatchResumeOutput::AwaitTool {
                result_ref,
                additional_context,
            },
        ) => await_resume_result(state, result_ref, additional_context)?,
        (
            ToolBatchSuspension::JoinedWorkflowCalls { .. },
            ToolBatchResumeOutput::JoinedWorkflowCalls { additional_context },
        ) => joined_workflow_resume_result(
            state,
            command.claim == WakeReason::Cancelled,
            &additional_context,
        )?,
        _ => {
            return Err(DomainError::InvariantViolation(
                "tool batch resume output does not match the parked suspension".to_owned(),
            ));
        }
    };
    validate_tool_batch_result(&result)?;
    validate_result_matches_active_tool_batch(state, &result, true)?;
    let joins = CoreAgentJoins {
        run_id: Some(result.run_id),
        turn_id: Some(result.turn_id),
        tool_batch_id: Some(result.batch_id),
        ..CoreAgentJoins::default()
    };
    let mut proposals = Vec::new();
    if command.claim == WakeReason::Cancelled
        && let ToolBatchSuspension::JoinedWorkflowCalls { calls, .. } = &parked.suspension
    {
        for joined in calls {
            if state
                .promises
                .promises
                .get(&joined.promise_id)
                .is_some_and(|promise| promise.status == PromiseStatus::Pending)
            {
                proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::Promise(PromiseEvent::Cancelled {
                        promise_id: joined.promise_id.clone(),
                    }),
                ));
            }
        }
    }
    proposals.push(CoreAgentEventProposal::new(
        joins.clone(),
        CoreAgentEvent::Tool(ToolEvent::BatchResumed {
            run_id: result.run_id,
            turn_id: result.turn_id,
            batch_id: result.batch_id,
        }),
    ));
    proposals.extend(tool_call_completed_proposals(state, None, result)?);
    Ok(proposals)
}

pub fn await_wake(state: &CoreAgentState, now_ms: u64) -> Option<WakeReason> {
    let active_run = state.runs.active.as_ref()?;
    let parked = active_run.parked_tool_batch.as_ref()?;
    if active_run.status == crate::RunStatus::Cancelling {
        return Some(WakeReason::Cancelled);
    }
    let spec = parked.suspension.spec();
    if spec
        .deadline_at_ms
        .is_some_and(|deadline| deadline <= now_ms)
    {
        return Some(WakeReason::Timeout);
    }
    if spec.promise_ids.is_empty() {
        return None;
    }
    let terminal = parked
        .suspension
        .spec()
        .promise_ids
        .iter()
        .filter_map(|promise_id| state.promises.promises.get(promise_id))
        .filter(|promise| promise.status.is_terminal())
        .count();
    match spec.mode {
        AwaitMode::All if terminal == spec.promise_ids.len() => Some(WakeReason::Terminal),
        AwaitMode::Any if terminal >= 1 => Some(WakeReason::Terminal),
        _ => None,
    }
}

fn validate_await_spec_for_active_run(
    state: &CoreAgentState,
    run_id: crate::RunId,
    spec: &AwaitSpec,
) -> Result<(), DomainError> {
    if spec.promise_ids.is_empty() {
        return Err(DomainError::InvariantViolation(
            "await requires at least one promise id".to_owned(),
        ));
    }
    for promise_id in &spec.promise_ids {
        let Some(promise) = state.promises.promises.get(promise_id) else {
            return Err(DomainError::InvariantViolation(format!(
                "unknown promise {}",
                promise_id
            )));
        };
        if promise.ownership != PromiseOwnership::Model {
            return Err(DomainError::InvariantViolation(format!(
                "promise {} is runtime-owned and cannot be awaited by the model",
                promise_id
            )));
        }
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

/// Supplemental entries ride beside a tool result; they may only be
/// user-role messages, never results or calls of their own.
fn validate_supplemental_entries(
    entries: &[ContextEntryInput],
    what: &dyn std::fmt::Display,
) -> Result<(), DomainError> {
    for entry in entries {
        if !matches!(
            entry.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ) {
            return Err(DomainError::InvariantViolation(format!(
                "{what} supplemental entries must be user messages, got {:?}",
                entry.kind
            )));
        }
    }
    Ok(())
}

fn await_resume_result(
    state: &CoreAgentState,
    result_ref: BlobRef,
    additional_context: Vec<ContextEntryInput>,
) -> Result<ToolInvocationBatchResult, DomainError> {
    validate_supplemental_entries(&additional_context, &"await")?;
    let active_run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
    let parked = active_run.parked_tool_batch.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation("await resume requires a parked tool batch".to_owned())
    })?;
    let ToolBatchSuspension::AwaitTool { call_id, .. } = &parked.suspension else {
        return Err(DomainError::InvariantViolation(
            "await resume requires an await-tool suspension".to_owned(),
        ));
    };
    let batch = active_run
        .tool_batches
        .get(&parked.batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {} is missing", parked.batch_id))
        })?;
    // The combined await result first, then whatever the awaited results
    // supplied, in the order the runtime prepared it.
    let mut model_visible_context_entries = vec![ToolInvocationResult::tool_result_context_entry(
        call_id,
        ToolCallStatus::Succeeded,
        result_ref.clone(),
    )];
    model_visible_context_entries.extend(additional_context);
    Ok(ToolInvocationBatchResult {
        run_id: active_run.run_id,
        turn_id: batch.turn_id,
        batch_id: batch.batch_id,
        results: vec![ToolInvocationResult {
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: Some(result_ref),
            model_visible_context_entries,
            error_ref: None,
            effects: Vec::new(),
        }],
    })
}

fn joined_workflow_resume_result(
    state: &CoreAgentState,
    cancel_pending: bool,
    additional_context: &[crate::PromiseContextEntries],
) -> Result<ToolInvocationBatchResult, DomainError> {
    let active_run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
    let parked = active_run.parked_tool_batch.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation("joined resume requires a parked tool batch".to_owned())
    })?;
    let ToolBatchSuspension::JoinedWorkflowCalls { calls, .. } = &parked.suspension else {
        return Err(DomainError::InvariantViolation(
            "joined resume requires a joined-workflow suspension".to_owned(),
        ));
    };
    let batch = active_run
        .tool_batches
        .get(&parked.batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {} is missing", parked.batch_id))
        })?;
    for supplement in additional_context {
        if !calls
            .iter()
            .any(|joined| joined.promise_id == supplement.promise_id)
        {
            return Err(DomainError::InvariantViolation(format!(
                "supplemental entries name promise {}, which no joined call of this batch owns",
                supplement.promise_id
            )));
        }
        validate_supplemental_entries(&supplement.entries, &supplement.promise_id)?;
    }
    let mut results = Vec::with_capacity(calls.len());
    for joined in calls {
        let promise = state
            .promises
            .promises
            .get(&joined.promise_id)
            .ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "joined Promise {} is missing",
                    joined.promise_id
                ))
            })?;
        if promise.ownership != PromiseOwnership::Runtime
            || (!promise.status.is_terminal() && !cancel_pending)
        {
            return Err(DomainError::InvariantViolation(format!(
                "joined Promise {} is not a terminal runtime-owned Promise",
                joined.promise_id
            )));
        }
        let (status, output_ref, error_ref, content_ref) = match promise.status {
            PromiseStatus::Resolved => (
                ToolCallStatus::Succeeded,
                promise.payload_ref.clone(),
                None,
                promise
                    .payload_ref
                    .clone()
                    .unwrap_or_else(crate::unavailable_tool_result_ref),
            ),
            PromiseStatus::Failed => {
                let error_ref = promise
                    .error_ref
                    .clone()
                    .unwrap_or_else(crate::unavailable_tool_result_ref);
                (
                    ToolCallStatus::Failed,
                    None,
                    Some(error_ref.clone()),
                    error_ref,
                )
            }
            PromiseStatus::Cancelled => {
                let error_ref = crate::unavailable_tool_result_ref();
                (
                    ToolCallStatus::Cancelled,
                    None,
                    Some(error_ref.clone()),
                    error_ref,
                )
            }
            PromiseStatus::Pending if cancel_pending => {
                let error_ref = crate::unavailable_tool_result_ref();
                (
                    ToolCallStatus::Cancelled,
                    None,
                    Some(error_ref.clone()),
                    error_ref,
                )
            }
            PromiseStatus::Pending => unreachable!("terminality was checked above"),
        };
        let mut model_visible_context_entries =
            vec![ToolInvocationResult::tool_result_context_entry(
                &joined.call_id,
                status,
                content_ref,
            )];
        if status == ToolCallStatus::Succeeded {
            model_visible_context_entries.extend(
                additional_context
                    .iter()
                    .filter(|item| item.promise_id == joined.promise_id)
                    .flat_map(|item| item.entries.iter().cloned()),
            );
        }
        results.push(ToolInvocationResult {
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: joined.call_id.clone(),
            status,
            output_ref,
            model_visible_context_entries,
            error_ref,
            effects: Vec::new(),
        });
    }
    Ok(ToolInvocationBatchResult {
        run_id: active_run.run_id,
        turn_id: batch.turn_id,
        batch_id: batch.batch_id,
        results,
    })
}

/// A tool result may only mint promise ids from its batch's slot: at or
/// above the base recorded when the batch was created, and never one the
/// session already holds. Executors number from
/// `ToolInvocationBatchRequest::promise_id_base`; this is the reducer-side
/// half of that contract.
fn validate_minted_promise_id(
    state: &CoreAgentState,
    run_id: crate::RunId,
    batch_id: crate::ToolBatchId,
    promise_id: &crate::PromiseId,
    minted_in_result: &mut BTreeSet<crate::PromiseId>,
) -> Result<(), DomainError> {
    let base = state
        .runs
        .active
        .as_ref()
        .filter(|active| active.run_id == run_id)
        .and_then(|active| active.tool_batches.get(&batch_id))
        .map(|batch| batch.promise_id_base)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "tool batch {batch_id} of run {run_id} is not active"
            ))
        })?;
    if promise_id.number() < base {
        return Err(DomainError::InvariantViolation(format!(
            "promise {promise_id} was minted below tool batch {batch_id}'s promise base {base}"
        )));
    }
    if state.promises.promises.contains_key(promise_id)
        || !minted_in_result.insert(promise_id.clone())
    {
        return Err(DomainError::InvariantViolation(format!(
            "promise {promise_id} already exists"
        )));
    }
    Ok(())
}

fn invalid_await_tool_result(call_id: ToolCallId, _message: String) -> ToolInvocationResult {
    let error_ref = crate::unavailable_tool_result_ref();
    ToolInvocationResult {
        duration_ms: None,
        output_bytes: None,
        truncated: false,
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
    let mut minted_promises = BTreeSet::new();
    let mut pending_port_emissions = BTreeMap::<crate::WorkflowToolId, u32>::new();
    let mut joined_calls = Vec::new();
    let mut joined_promise_proposals = Vec::new();
    let mut joined_tool_proposals = Vec::new();
    let mut saw_environment_selection_effect = false;
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
        let mut tool_proposals = Vec::new();
        let mut environment_proposals = Vec::new();
        let mut saw_port_effect = false;
        for effect in &result_item.effects {
            if let Some(event) =
                crate::core::components::environment::environment_event_from_effect(effect)?
            {
                // A sibling call completed earlier (per-call resumes) may
                // already have selected an environment; the exclusivity
                // invariant spans the whole batch, not one result set.
                if saw_environment_selection_effect
                    || batch_has_terminal_environment_selection(state, result.batch_id)
                {
                    return Err(DomainError::InvariantViolation(
                        "tool batch produced more than one environment selection effect".to_owned(),
                    ));
                }
                if result_item.status != ToolCallStatus::Succeeded {
                    return Err(DomainError::InvariantViolation(format!(
                        "failed tool call {} produced an environment selection effect",
                        call_id
                    )));
                }
                if state
                    .lifecycle
                    .config
                    .as_ref()
                    .and_then(|config| config.features.environments.as_ref())
                    .is_none()
                {
                    return Err(DomainError::InvariantViolation(
                        "environment selection effect requires the environments feature".to_owned(),
                    ));
                }
                saw_environment_selection_effect = true;
                environment_proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::Environment(event),
                ));
            }
            if let Some(promise) =
                crate::core::components::promise::promise_from_create_effect(effect, result.run_id)?
            {
                validate_minted_promise_id(
                    state,
                    result.run_id,
                    result.batch_id,
                    &promise.promise_id,
                    &mut minted_promises,
                )?;
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
                if promise.ownership != PromiseOwnership::Model {
                    return Err(DomainError::InvariantViolation(format!(
                        "promise {} is runtime-owned and cannot be cancelled by the model",
                        promise_id
                    )));
                }
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
                if promise.ownership != PromiseOwnership::Model {
                    return Err(DomainError::InvariantViolation(format!(
                        "promise {} is runtime-owned and cannot be detached by the model",
                        promise_id
                    )));
                }
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
                crate::core::components::workflow_tool::invocation_from_emit_effect(effect)?
            {
                if saw_port_effect {
                    return Err(DomainError::InvariantViolation(format!(
                        "tool call {} produced more than one workflow tool emission effect",
                        call_id
                    )));
                }
                saw_port_effect = true;
                if result_item.status != ToolCallStatus::Succeeded {
                    return Err(DomainError::InvariantViolation(format!(
                        "failed tool call {} produced a workflow tool emission effect",
                        call_id
                    )));
                }
                let session_id = session_id.ok_or_else(|| {
                    DomainError::InvariantViolation(
                        "workflow tool emission effect was admitted without session identity"
                            .to_owned(),
                    )
                })?;
                let pending = pending_port_emissions
                    .get(&invocation.tool_id)
                    .copied()
                    .unwrap_or(0);
                crate::core::components::workflow_tool::validate_emit_effect(
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
                    .insert(invocation.tool_id.clone(), pending.saturating_add(1));
                let binding = state
                    .workflow_tools
                    .bindings
                    .get(&invocation.tool_id)
                    .expect("binding was validated by validate_emit_effect");
                let is_joined = matches!(
                    binding.completion,
                    crate::WorkflowToolCompletion::Joined { .. }
                );
                if is_joined && result_item.effects.len() != 1 {
                    return Err(DomainError::InvariantViolation(
                        "joined workflow tool result must contain only its emission effect"
                            .to_owned(),
                    ));
                }
                // Keyed completion promises are created atomically with the
                // invocation, before its Emitted fact, so the Emitted apply
                // can verify every keyed promise exists with the canonical
                // producer-authorized source.
                if let Some(promises) = &invocation.completion_promises {
                    let deadline_ms =
                        crate::core::components::workflow_tool::completion_deadline_from_emit_effect(
                            effect,
                        )?;
                    if is_joined && deadline_ms.is_none_or(|deadline| deadline == 0) {
                        return Err(DomainError::InvariantViolation(
                            "joined workflow tool emission is missing its hard completion deadline"
                                .to_owned(),
                        ));
                    }
                    for (key, promise_id) in promises {
                        validate_minted_promise_id(
                            state,
                            result.run_id,
                            result.batch_id,
                            promise_id,
                            &mut minted_promises,
                        )?;
                        let source =
                            crate::core::components::workflow_tool::completion_promise_source(
                                binding,
                                &invocation,
                                key,
                            )?;
                        promise_proposals.push(CoreAgentEventProposal::new(
                            joins.clone(),
                            CoreAgentEvent::Promise(PromiseEvent::Created {
                                promise: crate::Promise {
                                    promise_id: promise_id.clone(),
                                    source,
                                    scope: crate::PromiseScope::Run {
                                        run_id: result.run_id,
                                    },
                                    ownership: if is_joined {
                                        PromiseOwnership::Runtime
                                    } else {
                                        PromiseOwnership::Model
                                    },
                                    status: PromiseStatus::Pending,
                                    payload_ref: None,
                                    error_ref: None,
                                    deadline_ms,
                                },
                            }),
                        ));
                    }
                }
                // The trusted effect is one carrier; the durable event
                // family follows the binding's target lifecycle.
                if is_joined {
                    let promise_id = invocation
                        .completion_promises
                        .as_ref()
                        .and_then(|promises| promises.get(crate::REPLY_COMPLETION_KEY))
                        .cloned()
                        .expect("Joined invocation validation requires one reply Promise");
                    joined_calls.push(crate::JoinedWorkflowCall {
                        call_id: call_id.clone(),
                        invocation_id: invocation.invocation_id.clone(),
                        promise_id,
                    });
                }
                let event = match &binding.target {
                    crate::WorkflowToolTarget::Start { start } => {
                        let execution_id = crate::workflow_tool_execution_id(
                            &invocation.invocation_id,
                            &start.recipe_fingerprint,
                        );
                        crate::WorkflowToolEvent::StartRequested {
                            invocation,
                            execution_id,
                        }
                    }
                    crate::WorkflowToolTarget::Bound { .. } => {
                        crate::WorkflowToolEvent::Emitted { invocation }
                    }
                };
                tool_proposals.push(CoreAgentEventProposal::new(
                    joins.clone(),
                    CoreAgentEvent::WorkflowTool(event),
                ));
                if is_joined {
                    joined_promise_proposals.append(&mut promise_proposals);
                    joined_tool_proposals.append(&mut tool_proposals);
                }
            }
        }
        let is_joined_call = joined_calls
            .last()
            .is_some_and(|joined| joined.call_id == call_id);
        if !is_joined_call {
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
            proposals.extend(tool_proposals);
            proposals.extend(environment_proposals);
        } else if !promise_proposals.is_empty()
            || !tool_proposals.is_empty()
            || !environment_proposals.is_empty()
        {
            return Err(DomainError::InvariantViolation(
                "joined workflow tool result mixed joined and unrelated effects".to_owned(),
            ));
        }
    }
    if !joined_calls.is_empty() {
        proposals.extend(joined_promise_proposals);
        let joins = CoreAgentJoins {
            run_id: Some(result.run_id),
            turn_id: Some(result.turn_id),
            tool_batch_id: Some(result.batch_id),
            ..CoreAgentJoins::default()
        };
        let promise_ids = joined_calls
            .iter()
            .map(|call| call.promise_id.clone())
            .collect();
        proposals.push(CoreAgentEventProposal::new(
            joins,
            CoreAgentEvent::Tool(ToolEvent::BatchDeferred {
                run_id: result.run_id,
                turn_id: result.turn_id,
                batch_id: result.batch_id,
                suspension: ToolBatchSuspension::JoinedWorkflowCalls {
                    calls: joined_calls,
                    spec: AwaitSpec {
                        promise_ids,
                        mode: AwaitMode::All,
                        deadline_at_ms: None,
                    },
                },
            }),
        ));
        proposals.extend(joined_tool_proposals);
    }
    Ok(proposals)
}

fn batch_has_terminal_environment_selection(state: &CoreAgentState, batch_id: ToolBatchId) -> bool {
    state
        .runs
        .active
        .as_ref()
        .and_then(|active_run| active_run.tool_batches.get(&batch_id))
        .is_some_and(|batch| {
            batch.calls.iter().any(|call_state| {
                call_state.result.as_ref().is_some_and(|result| {
                    result
                        .effects
                        .iter()
                        .any(crate::core::components::environment::is_environment_selection_effect)
                })
            })
        })
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
        .parked_tool_batch
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
        duration_ms: result.duration_ms,
        output_bytes: result.output_bytes,
        truncated: result.truncated,
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
        ProviderApiKind, RunConfig, RunFailureKind, RunId, RunRequestCommand, RunRequestSource,
        RunStatus, SessionConfig, TokenEstimate, TokenEstimateQuality, ToolBatchOutcome,
        ToolChoice, ToolEffect, ToolInvocationResult, ToolKind, ToolName, ToolParallelism,
        ToolSpec, WorkflowEndpointRef, WorkflowToolDefinition, WorkflowToolId,
        WorkflowToolInvocation,
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

    #[test]
    fn input_origin_survives_run_and_steering_replay_and_legacy_events() {
        let mut drive = CoreAgentDrive::from_replayed(
            SessionId::new("origin-test"),
            CoreAgentState::new(),
            None,
        );
        open_session(&mut drive);
        let checkpoint = drive.state().clone();
        let mut input = user_input(BlobRef::from_bytes(b"hello"));
        input[0].origin = Some("user:operator".into());
        let action = drive
            .admit_command(request_run_command(None, input, run_config()), 20)
            .unwrap();
        let mut log = commit_action(&mut drive, action);
        for at in [21, 22] {
            let action = drive.next_action(at, 64).unwrap();
            log.extend(commit_action(&mut drive, action));
        }
        let mut input = user_input(BlobRef::from_bytes(b"automated steering"));
        input[0].origin = Some("event".into());
        let action = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: crate::RunId::new(1),
                    input,
                },
                23,
            )
            .unwrap();
        log.extend(commit_action(&mut drive, action));
        let action = drive.next_action(24, 64).unwrap();
        log.extend(commit_action(&mut drive, action));
        let origins: Vec<_> = drive
            .state()
            .context
            .entries
            .iter()
            .map(|entry| entry.origin.as_deref())
            .collect();
        assert_eq!(origins, vec![Some("user:operator"), Some("event")]);
        let mut replayed = checkpoint.clone();
        for entry in &log {
            let stored = CoreAgentCodec.encode_entry(entry).unwrap();
            crate::apply_event(
                &mut replayed,
                &CoreAgentCodec.decode_entry(&stored).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(&replayed, drive.state());
        // Old event payloads and checkpoints omit origin. They still replay as unknown.
        fn remove_origin(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    map.remove("origin");
                    for value in map.values_mut() {
                        remove_origin(value);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        remove_origin(value);
                    }
                }
                _ => {}
            }
        }
        let mut legacy = checkpoint;
        for entry in &log {
            let mut stored = CoreAgentCodec.encode_entry(entry).unwrap();
            remove_origin(&mut stored.event.payload);
            crate::apply_event(&mut legacy, &CoreAgentCodec.decode_entry(&stored).unwrap())
                .unwrap();
        }
        assert!(
            legacy
                .context
                .entries
                .iter()
                .all(|entry| entry.origin.is_none())
        );
        let checkpoint_json = serde_json::to_value(drive.state()).unwrap();
        let restored: CoreAgentState = serde_json::from_value(checkpoint_json).unwrap();
        assert_eq!(&restored, drive.state());
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
            content: crate::ContentRef {
                content_ref: BlobRef::from_bytes(content),
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
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
            content: crate::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    fn provider_opaque_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::ProviderOpaque,
            content: crate::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
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
            content: crate::ContentRef {
                content_ref,
                media_type: Some("application/json".to_owned()),
                provider_kind: Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND.to_owned()),
            },
            preview: Some("OpenAI Responses compaction item".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    fn instruction_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Instructions,
            content: crate::ContentRef::text(content_ref),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    const TEST_CATALOG_KEY: &str = "test.catalog";

    fn catalog_input(content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Catalog {
                title: "Test catalog".to_owned(),
            },
            content: crate::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    fn drive_until_generate(drive: &mut CoreAgentDrive) -> LlmGenerationRequest {
        drive_until_generate_with_planned_event(drive).1
    }

    fn test_tool_id(name: &str) -> ToolName {
        ToolName::new(match name {
            "await" => "concurrency.await",
            "cancel" => "concurrency.cancel",
            "detach" => "concurrency.detach",
            "mcp_echo__hello" => "mcp_echo",
            other => other,
        })
    }

    fn test_tool_spec(tool_name: &str) -> ToolSpec {
        ToolSpec {
            name: test_tool_id(tool_name),
            execution: Default::default(),
            kind: ToolKind::Function(FunctionToolSpec {
                description_ref: None,
                input_schema_ref: BlobRef::from_bytes(br#"{"type":"object"}"#),
                output_schema_ref: None,
                strict: None,
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
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
            tool_id: Some(test_tool_id(tool_name)),
            tool_name: ToolName::new(tool_name),
            provider_kind: None,
            arguments_ref: BlobRef::from_bytes(br#"{"wait":true}"#),
            native_call_ref: None,
        };
        drive_until_tool_batch_request_with_calls(drive, request, vec![tool_call])
    }

    fn drive_until_tool_batch_request_with_calls(
        drive: &mut CoreAgentDrive,
        request: LlmGenerationRequest,
        tool_calls: Vec<ObservedToolCall>,
    ) -> ToolInvocationBatchRequest {
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: Some("resp-tool".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls,
                        approval_requests: Vec::new(),
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

    #[test]
    fn tool_batch_carries_admitted_runtime_policies_and_active_environment() {
        let session_id = SessionId::new("session-environment-runtime");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let mut session_config = config();
        session_config.features.environments = Some(crate::EnvironmentsFeature {
            providers: Some(vec!["provider-a".to_owned(), "provider-b".to_owned()]),
            selection_tools: true,
            ..crate::EnvironmentsFeature::default()
        });
        session_config.features.subagents = Some(test_subagents_feature());
        open_session_with_config(&mut drive, session_config);
        let set_active = drive
            .admit_command(
                CoreAgentCommand::SetActiveEnvironment {
                    environment_id: crate::EnvironmentId::new("environment-a"),
                },
                11,
            )
            .expect("set active environment");
        commit_action(&mut drive, set_active);
        install_test_tool(&mut drive, "environment_read");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let request = drive_until_tool_batch_request(&mut drive, generation, "environment_read");

        assert_eq!(
            request.active_environment_id,
            Some(crate::EnvironmentId::new("environment-a"))
        );
        assert_eq!(
            request.environment_policy,
            Some(crate::EnvironmentPolicyRuntime::new(
                Some(vec!["provider-a".to_owned(), "provider-b".to_owned()]),
                None,
            ))
        );
        assert_eq!(request.subagents_policy, Some(test_subagents_feature()));
    }

    fn test_subagents_feature() -> crate::SubagentsFeature {
        crate::SubagentsFeature {
            agents: vec![crate::SubagentAgentConfig {
                profile_id: "reviewer".to_owned(),
            }],
            ..crate::SubagentsFeature::default()
        }
    }

    fn completed_tool_result(request: &ToolInvocationBatchRequest) -> ToolInvocationBatchResult {
        ToolInvocationBatchResult {
            run_id: request.run_id,
            turn_id: request.turn_id,
            batch_id: request.batch_id,
            results: vec![ToolInvocationResult {
                duration_ms: None,
                output_bytes: None,
                truncated: false,
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

    /// The promise the shared parked-await fixtures wait on; an await must
    /// name at least one run-scoped, model-owned promise.
    const WAIT_PROMISE_ID: &str = "promise_1";

    fn wait_await_spec() -> AwaitSpec {
        AwaitSpec {
            promise_ids: vec![crate::PromiseId::new(WAIT_PROMISE_ID)],
            mode: AwaitMode::All,
            deadline_at_ms: Some(90),
        }
    }

    fn insert_wait_promise(drive: &mut CoreAgentDrive, run_id: crate::RunId) {
        let promise_id = crate::PromiseId::new(WAIT_PROMISE_ID);
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id,
                source: crate::PromiseSource::Timer {
                    fire_at_ms: u64::MAX,
                },
                scope: crate::PromiseScope::Run { run_id },
                ownership: crate::PromiseOwnership::Model,
                status: crate::PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );
    }

    /// Park the single tool invocation on the shared wait promise.
    fn park_on_wait_promise(
        drive: &mut CoreAgentDrive,
        request: &ToolInvocationBatchRequest,
    ) -> ToolBatchOutcome {
        insert_wait_promise(drive, request.run_id);
        deferred_await_outcome(request)
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

    fn resume_tool_batch_command(request: &ToolInvocationBatchRequest) -> CoreAgentCommand {
        resume_tool_batch_command_with_claim(request, WakeReason::Timeout)
    }

    fn resume_tool_batch_command_with_claim(
        request: &ToolInvocationBatchRequest,
        claim: WakeReason,
    ) -> CoreAgentCommand {
        CoreAgentCommand::ResumeToolBatch(crate::ResumeToolBatchCommand {
            run_id: request.run_id,
            batch_id: request.batch_id,
            claim,
            claim_observed_at_ms: 91,
            output: ToolBatchResumeOutput::AwaitTool {
                result_ref: BlobRef::from_bytes(b"await output"),
                additional_context: Vec::new(),
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
    fn native_run_output_replays_from_recorded_events_without_blob_access() {
        let session_id = SessionId::new("native-output-replay");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session(&mut drive);
        let action = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    run_config(),
                ),
                20,
            )
            .unwrap();
        commit_action(&mut drive, action);
        let request = drive_until_generate(&mut drive);
        let checkpoint = serde_json::to_vec(drive.state()).unwrap();
        let head = drive.head().cloned();
        let mut message = message_input(
            ContextMessageRole::Assistant,
            BlobRef::from_bytes(b"opaque native bytes"),
        );
        message.content.media_type = Some("application/json".into());
        message.content.provider_kind = Some("provider.message".into());
        message.provenance_ref = Some(BlobRef::from_bytes(b"origin artifact"));
        let expected = message.content.clone();
        let action = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![message],
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: None,
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: vec![],
                        approval_requests: vec![],
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .unwrap();
        let mut events = commit_action(&mut drive, action);
        for now in 31..40 {
            let action = drive.next_action(now, 64).unwrap();
            if matches!(action, CoreAgentAction::Idle) {
                break;
            }
            events.extend(commit_action(&mut drive, action));
        }
        assert_eq!(
            drive.state().runs.completed.last().unwrap().output.as_ref(),
            Some(&expected)
        );
        let mut replayed = CoreAgentDrive::from_replayed(
            session_id,
            serde_json::from_slice(&checkpoint).unwrap(),
            head,
        );
        replayed
            .resume_appended(
                events
                    .iter()
                    .map(|entry| CoreAgentCodec.encode_entry(entry).unwrap())
                    .collect(),
            )
            .unwrap();
        assert_eq!(replayed.state(), drive.state());
        assert_eq!(
            replayed
                .state()
                .runs
                .completed
                .last()
                .unwrap()
                .output
                .as_ref(),
            Some(&expected)
        );
    }

    #[test]
    fn final_output_retains_native_encoding_without_interpreting_payload() {
        let mut message = message_input(
            ContextMessageRole::Assistant,
            BlobRef::from_bytes(b"native payload"),
        );
        message.content.media_type = Some("application/json".to_owned());
        message.content.provider_kind = Some("provider.native_message".to_owned());
        let output = final_output(&[message.clone()]).expect("final output");
        assert_eq!(output, message.content.clone());
        let event = crate::RunEvent::Completed {
            run_id: RunId::new(1),
            output: Some(output.clone()),
        };
        let encoded = serde_json::to_vec(&event).expect("encode completion");
        let decoded: crate::RunEvent = serde_json::from_slice(&encoded).expect("decode completion");
        assert_eq!(decoded, event);
        // A run owns a durable descriptor, independent of active context entries.
        message.content.content_ref = BlobRef::from_bytes(b"replacement");
        assert_ne!(output.content_ref, message.content.content_ref);
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
    fn provider_opaque_approval_request_is_never_a_final_output() {
        let opaque_ref = BlobRef::from_bytes(b"mcp approval request");
        let entries = vec![provider_opaque_input(opaque_ref)];

        assert_eq!(final_output(&entries), None);
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

    fn upsert(drive: &mut CoreAgentDrive, key: &str, entry: ContextEntryInput, at: u64) {
        let action = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new(key),
                    entry,
                },
                at,
            )
            .expect("context upsert");
        commit_action(drive, action);
    }

    fn client_catalog_input(title: &str, content_ref: BlobRef) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Catalog {
                title: title.to_owned(),
            },
            content: crate::ContentRef {
                content_ref,
                media_type: Some("text/markdown".to_owned()),
                provider_kind: None,
            },
            preview: Some(title.to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    fn entry_ids(drive: &CoreAgentDrive) -> Vec<u64> {
        drive
            .state()
            .context
            .entries
            .iter()
            .map(|entry| entry.entry_id.as_u64())
            .collect()
    }

    #[test]
    fn keyed_catalog_upsert_supersedes_and_keeps_the_previous_version() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        upsert(
            &mut drive,
            TEST_CATALOG_KEY,
            catalog_input(BlobRef::from_bytes(b"v1")),
            20,
        );
        upsert(
            &mut drive,
            "client.native",
            provider_opaque_input(BlobRef::from_bytes(b"hello")),
            21,
        );
        upsert(
            &mut drive,
            TEST_CATALOG_KEY,
            catalog_input(BlobRef::from_bytes(b"v2")),
            30,
        );

        // v1 (1), the client entry (2), v2 (3): nothing before v2 moved.
        assert_eq!(entry_ids(&drive), vec![1, 2, 3]);
        let state = drive.state();
        let v1 = &state.context.entries[0];
        let v2 = &state.context.entries[2];
        assert_eq!(v1.supersedes, None);
        assert_eq!(v2.supersedes, Some(v1.entry_id));
        assert!(crate::is_superseded_context_entry(state, v1.entry_id));
        assert!(!crate::is_superseded_context_entry(state, v2.entry_id));
        assert_eq!(
            crate::current_context_entry(state, &ContextEntryKey::new(TEST_CATALOG_KEY))
                .map(|entry| entry.entry_id),
            Some(v2.entry_id)
        );

        // Both versions render, in id order; only the stale one is compactable.
        let planned = crate::core::components::context::planned_context_entry_ids(state);
        assert_eq!(
            planned.iter().map(|id| id.as_u64()).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let compactable = crate::core::components::context::compactable_context_entry_ids(state);
        assert!(compactable.contains(&v1.entry_id));
        assert!(!compactable.contains(&v2.entry_id));

        // An identical put is a no-op.
        let noop = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new(TEST_CATALOG_KEY),
                    entry: catalog_input(BlobRef::from_bytes(b"v2")),
                },
                40,
            )
            .expect("no-op upsert");
        assert!(matches!(noop, CoreAgentAction::Idle));
    }

    #[test]
    fn catalog_keys_supersede_independently_and_replay_text_and_provenance() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("catalogs"), CoreAgentState::new(), None);
        open_session(&mut drive);
        let checkpoint = drive.state().clone();
        let mut first = client_catalog_input("Menu", BlobRef::from_bytes(b"old text"));
        first.provenance_ref = Some(BlobRef::from_bytes(b"old source"));
        let mut second = client_catalog_input("Menu", BlobRef::from_bytes(b"new text"));
        second.provenance_ref = Some(BlobRef::from_bytes(b"new source"));
        let mut log = Vec::new();
        for (index, (key, entry)) in [
            ("runtime.catalog.a", first.clone()),
            ("client.catalog", first.clone()),
            ("runtime.catalog.a", second.clone()),
        ]
        .into_iter()
        .enumerate()
        {
            let action = drive
                .admit_command(
                    CoreAgentCommand::UpsertContext {
                        expected_revision: None,
                        key: ContextEntryKey::new(key),
                        entry,
                    },
                    20 + index as u64,
                )
                .unwrap();
            log.extend(commit_action(&mut drive, action));
        }
        let expected = std::collections::BTreeMap::from([
            (ContextEntryKey::new("runtime.catalog.a"), second),
            (ContextEntryKey::new("client.catalog"), first.clone()),
        ]);
        assert_eq!(crate::current_catalog_inputs(drive.state()), expected);
        let entries = &drive.state().context.entries;
        assert_eq!(entries[1].supersedes, None);
        assert_eq!(entries[2].supersedes, Some(entries[0].entry_id));
        assert_eq!(entries[0].content, first.content);
        assert_eq!(entries[0].provenance_ref, first.provenance_ref);

        let mut replayed = checkpoint;
        for entry in &log {
            let stored = CoreAgentCodec.encode_entry(entry).unwrap();
            crate::apply_event(
                &mut replayed,
                &CoreAgentCodec.decode_entry(&stored).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(&replayed, drive.state());
        assert_eq!(crate::current_catalog_inputs(&replayed), expected);
        let action = drive
            .admit_command(
                CoreAgentCommand::RemoveContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("runtime.catalog.a"),
                },
                30,
            )
            .unwrap();
        commit_action(&mut drive, action);
        assert_eq!(
            crate::current_catalog_inputs(drive.state()),
            std::collections::BTreeMap::from([(ContextEntryKey::new("client.catalog"), first)])
        );
    }

    #[test]
    fn superseded_catalog_versions_are_capped_oldest_first() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        for version in 0..(crate::SUPERSEDED_CATALOG_CAP as u64 + 3) {
            upsert(
                &mut drive,
                "bot:directory",
                client_catalog_input(
                    "Bot directory",
                    BlobRef::from_bytes(version.to_string().as_bytes()),
                ),
                20 + version,
            );
        }
        let ids = entry_ids(&drive);
        assert_eq!(ids.len(), crate::SUPERSEDED_CATALOG_CAP + 1);
        assert_eq!(
            ids.first(),
            Some(&3),
            "the two oldest versions were dropped"
        );
        assert_eq!(
            ids.last(),
            Some(&(crate::SUPERSEDED_CATALOG_CAP as u64 + 3))
        );
        let state = drive.state();
        for pair in state.context.entries.windows(2) {
            assert_eq!(pair[1].supersedes, Some(pair[0].entry_id));
        }
    }

    #[test]
    fn remove_context_clears_every_catalog_version_under_the_key() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        upsert(
            &mut drive,
            "bot:directory",
            client_catalog_input("Bot directory", BlobRef::from_bytes(b"a")),
            20,
        );
        upsert(
            &mut drive,
            "bot:directory",
            client_catalog_input("Bot directory", BlobRef::from_bytes(b"b")),
            21,
        );
        assert_eq!(drive.state().context.entries.len(), 2);
        let action = drive
            .admit_command(
                CoreAgentCommand::RemoveContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("bot:directory"),
                },
                22,
            )
            .expect("remove");
        commit_action(&mut drive, action);
        assert!(drive.state().context.entries.is_empty());
    }

    #[test]
    fn keyed_non_catalog_upsert_still_replaces_in_place() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        upsert(
            &mut drive,
            "client.native",
            provider_opaque_input(BlobRef::from_bytes(b"a")),
            20,
        );
        upsert(
            &mut drive,
            "client.native",
            provider_opaque_input(BlobRef::from_bytes(b"b")),
            21,
        );
        let state = drive.state();
        assert_eq!(state.context.entries.len(), 1);
        assert_eq!(
            state.context.entries[0].content.content_ref,
            BlobRef::from_bytes(b"b")
        );
        assert_eq!(state.context.entries[0].supersedes, None);
    }

    #[test]
    fn catalog_is_context_only_and_needs_a_title() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        let input = client_catalog_input("Bot directory", BlobRef::from_bytes(b"a"));

        drive
            .admit_command(
                request_run_command(None, vec![input.clone()], run_config()),
                20,
            )
            .expect_err("a catalog is not run input");
        drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    expected_revision: None,
                    key: ContextEntryKey::new("bot:directory"),
                    entry: client_catalog_input("   ", BlobRef::from_bytes(b"a")),
                },
                22,
            )
            .expect_err("a catalog needs a title");

        upsert(&mut drive, "bot:directory", input, 23);
        assert!(matches!(
            drive.state().context.entries[0].kind,
            ContextEntryKind::Catalog { ref title } if title == "Bot directory"
        ));
    }

    #[test]
    fn planned_context_includes_catalogs_at_their_positions() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        let catalog = ContextEntryInput {
            kind: ContextEntryKind::Catalog {
                title: "Agents".to_owned(),
            },
            content: crate::ContentRef {
                content_ref: BlobRef::from_bytes(b"agents"),
                media_type: Some("application/json".to_owned()),
                provider_kind: None,
            },
            preview: Some("Sub-agent catalog".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        };
        upsert(&mut drive, "agents.menu", catalog, 20);
        upsert(
            &mut drive,
            "client.native",
            provider_opaque_input(BlobRef::from_bytes(b"hello")),
            21,
        );
        let planned = crate::core::components::context::planned_context_entry_ids(drive.state());
        assert_eq!(
            planned.iter().map(|id| id.as_u64()).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn standalone_compaction_prunes_superseded_catalogs_and_keeps_the_current_one() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(None, Some(256)));
        upsert(
            &mut drive,
            TEST_CATALOG_KEY,
            catalog_input(BlobRef::from_bytes(b"v1")),
            20,
        );
        upsert(
            &mut drive,
            "client.native",
            provider_opaque_input(BlobRef::from_bytes(b"native")),
            21,
        );
        upsert(
            &mut drive,
            TEST_CATALOG_KEY,
            catalog_input(BlobRef::from_bytes(b"v2")),
            22,
        );
        assert_eq!(entry_ids(&drive), vec![1, 2, 3]);

        let request_compaction = drive
            .admit_command(CoreAgentCommand::CompactContext, 30)
            .expect("manual compaction");
        commit_action(&mut drive, request_compaction);
        let CoreAgentAction::CompactContext { request } =
            drive.next_action(31, 64).expect("compact action")
        else {
            panic!("expected compact action");
        };
        // The stale catalog version and the conversation go to the compactor;
        // the current catalog stays out of it.
        assert_eq!(
            request
                .request
                .context
                .entry_ids()
                .iter()
                .map(|id| id.as_u64())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let completed = drive
            .resume_context_compaction(
                ContextCompactionResult {
                    session_id: request.session_id,
                    context_revision: request.request.context.context_revision,
                    status: ContextCompactionStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![openai_compaction_input(BlobRef::from_bytes(
                        br#"{"type":"compaction","encrypted_content":"opaque"}"#,
                    ))],
                },
                32,
            )
            .expect("resume compaction");
        commit_action(&mut drive, completed);
        let prune = drive.next_action(33, 64).expect("prune compacted entries");
        commit_action(&mut drive, prune);

        // v1 and the native entry are gone; v2 (id 3) and the compaction item remain.
        let ids = entry_ids(&drive);
        assert_eq!(ids, vec![3, 4]);
        assert!(matches!(
            drive.state().context.entries[0].kind,
            ContextEntryKind::Catalog { .. }
        ));
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
        assert_eq!(entry.content.content_ref, content_ref);
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
        assert_eq!(items[0].content.content_ref, first_ref);
        assert!(matches!(items[1].kind, ContextEntryKind::Instructions));
        assert_eq!(items[1].content.content_ref, second_ref);
        assert!(matches!(
            items[2].kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(items[2].content.content_ref, input_ref);
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
                        duration_ms: None,
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
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
            retained[0].content.provider_kind.as_deref(),
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
            drive.state().context.entries[0]
                .content
                .provider_kind
                .as_deref(),
            Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND)
        );
    }

    #[test]
    fn skill_reads_and_inserted_text_are_compacted_normally_and_replay() {
        let session_id = SessionId::new("skill-read-compaction");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session_with_config(&mut drive, standalone_compaction_config(None, Some(256)));
        upsert(
            &mut drive,
            "instructions.required",
            instruction_input(BlobRef::from_bytes(b"Required guidance")),
            12,
        );
        upsert(
            &mut drive,
            TEST_CATALOG_KEY,
            catalog_input(BlobRef::from_bytes(b"Skill menu")),
            13,
        );
        install_test_tool(&mut drive, "vfs.read_file");
        let inserted_ref =
            BlobRef::from_bytes(b"# Review\nRead the release notes before reviewing.");
        request_run(&mut drive, inserted_ref.clone());
        let request = drive_until_generate(&mut drive);
        let batch = drive_until_tool_batch_request_with_calls(
            &mut drive,
            request,
            vec![ObservedToolCall {
                call_id: crate::ToolCallId::new("read-skill"),
                tool_id: Some(ToolName::new("vfs.read_file")),
                tool_name: ToolName::new("vfs_read_file"),
                provider_kind: None,
                arguments_ref: BlobRef::from_bytes(br#"{"path":"/skills/review/SKILL.md"}"#),
                native_call_ref: None,
            }],
        );
        let read_ref = BlobRef::from_bytes(b"# Review\nCheck the rollback plan.");
        let mut result = completed_tool_result(&batch);
        result.results[0].output_ref = Some(read_ref.clone());
        result.results[0].model_visible_context_entries =
            vec![ToolInvocationResult::tool_result_context_entry(
                &batch.calls[0].call_id,
                ToolCallStatus::Succeeded,
                read_ref.clone(),
            )];
        result.results[0].effects.clear();
        let action = drive
            .resume_tool_batch_outcome(ToolBatchOutcome::completed(result), 120)
            .unwrap();
        commit_action(&mut drive, action);
        let request = drive_until_generate(&mut drive);
        let action = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![message_input(
                        ContextMessageRole::Assistant,
                        BlobRef::from_bytes(b"Reviewed."),
                    )],
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: None,
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: vec![],
                        approval_requests: vec![],
                        context_token_estimate: None,
                    },
                },
                130,
            )
            .unwrap();
        commit_action(&mut drive, action);
        for now in 131..160 {
            let action = drive.next_action(now, 64).unwrap();
            if matches!(action, CoreAgentAction::Idle) {
                break;
            }
            commit_action(&mut drive, action);
        }
        assert!(drive.state().runs.active.is_none());
        let checkpoint = serde_json::to_vec(drive.state()).unwrap();
        let head = drive.head().cloned();
        let action = drive
            .admit_command(CoreAgentCommand::CompactContext, 160)
            .unwrap();
        let mut events = commit_action(&mut drive, action);
        let CoreAgentAction::CompactContext { request } = drive.next_action(161, 64).unwrap()
        else {
            panic!("expected ordinary compaction");
        };
        let entries = &request.request.context.entries;
        assert!(
            entries
                .iter()
                .any(|entry| entry.content.content_ref == inserted_ref)
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.content.content_ref == read_ref
                    && matches!(entry.kind, ContextEntryKind::ToolResult { .. }))
        );
        assert!(entries.iter().all(|entry| !matches!(
            entry.kind,
            ContextEntryKind::Catalog { .. } | ContextEntryKind::Instructions
        )));
        let action = drive
            .resume_context_compaction(
                ContextCompactionResult {
                    session_id: request.session_id,
                    context_revision: request.request.context.context_revision,
                    status: ContextCompactionStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![openai_compaction_input(BlobRef::from_bytes(
                        br#"{"type":"compaction","encrypted_content":"summary"}"#,
                    ))],
                },
                162,
            )
            .unwrap();
        events.extend(commit_action(&mut drive, action));
        let action = drive.next_action(163, 64).unwrap();
        events.extend(commit_action(&mut drive, action));
        assert_eq!(drive.state().context.entries.len(), 3);
        assert!(
            drive
                .state()
                .context
                .entries
                .iter()
                .all(|entry| entry.content.content_ref != read_ref
                    && entry.content.content_ref != inserted_ref)
        );
        let mut replayed = CoreAgentDrive::from_replayed(
            session_id,
            serde_json::from_slice(&checkpoint).unwrap(),
            head,
        );
        replayed
            .resume_appended(
                events
                    .iter()
                    .map(|entry| CoreAgentCodec.encode_entry(entry).unwrap())
                    .collect(),
            )
            .unwrap();
        assert_eq!(replayed.state(), drive.state());
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

    /// Steering admitted while a turn is generating stays unmaterialized
    /// until that turn completes (its request is frozen at the planned
    /// context revision and the runtime re-derives it from state); it then
    /// lands before the next turn, in admission order.
    #[test]
    fn steering_rejects_a_previous_run_target_and_replays_the_matching_target() {
        let mut drive = CoreAgentDrive::from_replayed(
            SessionId::new("steering-target"),
            CoreAgentState::new(),
            None,
        );
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"first"));
        let first = drive_until_generate(&mut drive).run_id;
        let cancel = drive
            .admit_command(CoreAgentCommand::ForceCancelRun { run_id: first }, 40)
            .unwrap();
        commit_action(&mut drive, cancel);
        request_run(&mut drive, BlobRef::from_bytes(b"second"));
        let second = drive_until_generate(&mut drive).run_id;
        assert_ne!(first, second);
        let checkpoint = drive.state().clone();
        let error = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: first,
                    input: user_input(BlobRef::from_bytes(b"stale")),
                },
                50,
            )
            .unwrap_err();
        assert!(
            matches!(error, CoreAgentDriveError::Command(CommandError::Rejected(rejection))
            if rejection.kind == crate::CommandRejectionKind::UnknownReference)
        );
        assert_eq!(drive.state(), &checkpoint);
        let valid = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: second,
                    input: user_input(BlobRef::from_bytes(b"current")),
                },
                51,
            )
            .unwrap();
        let entries = commit_action(&mut drive, valid);
        let mut replayed = checkpoint;
        for entry in entries {
            let stored = CoreAgentCodec.encode_entry(&entry).unwrap();
            crate::apply_event(
                &mut replayed,
                &CoreAgentCodec.decode_entry(&stored).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(&replayed, drive.state());
        assert_eq!(
            drive.state().runs.active.as_ref().unwrap().steering.len(),
            1
        );
    }

    #[test]
    fn steering_materializes_after_in_flight_turn_completes() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        install_test_tool(&mut drive, "await");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(&mut drive);
        assert_eq!(openai_items(&request).len(), 1);

        let steering_one = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: crate::RunId::new(1),
                    input: user_input(BlobRef::from_bytes(b"steering one")),
                },
                30,
            )
            .expect("steering one");
        commit_action(&mut drive, steering_one);
        let steering_two = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: crate::RunId::new(1),
                    input: user_input(BlobRef::from_bytes(b"steering two")),
                },
                31,
            )
            .expect("steering two");
        commit_action(&mut drive, steering_two);

        // In flight: the drive re-issues the same generation request (the
        // hosted runtime re-derives it from state) and nothing materializes.
        let reissued = drive.next_action(32, 64).expect("next action");
        let CoreAgentAction::GenerateLlm { request: again } = reissued else {
            panic!("expected the pending generation, got {reissued:?}");
        };
        assert_eq!(
            again.request.request_fingerprint,
            request.request.request_fingerprint
        );
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(active_run.steering.len(), 2);
        assert!(
            active_run
                .steering
                .iter()
                .all(|steering| steering.entry_ids.is_empty())
        );

        // The turn completes with a tool call; steering materializes after
        // the tool results, before the next turn.
        let batch = drive_until_tool_batch_request(&mut drive, request, "await");
        let resumed = drive
            .resume_tool_batch(completed_tool_result(&batch), 90)
            .expect("tool result");
        commit_action(&mut drive, resumed);
        let next_request = drive_until_generate(&mut drive);
        let steering_sources = next_request
            .request
            .context
            .entries
            .iter()
            .filter_map(|entry| match entry.source {
                ContextEntrySource::Steering { steering_id, .. } => Some(steering_id.as_u64()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(steering_sources, vec![1, 2]);
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(active_run.steering[0].entry_ids.len(), 1);
        assert_eq!(active_run.steering[1].entry_ids.len(), 1);
        assert_eq!(
            active_run.steering[0].consumed_by_turn_id,
            Some(next_request.turn_id)
        );
        assert_eq!(
            active_run.steering[1].consumed_by_turn_id,
            Some(next_request.turn_id)
        );
    }

    /// A parked run (model-chosen `await`) accepts steering without
    /// waking; the steering materializes after the batch resumes and is part
    /// of the next generation request.
    #[test]
    fn steering_parked_run_is_accepted_and_materializes_on_resume() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let parked = park_on_wait_promise(&mut drive, &request);
        let deferred = drive
            .resume_tool_batch_outcome(parked, 90)
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);
        assert_eq!(
            drive.state().runs.active.as_ref().expect("active").status,
            RunStatus::Parked
        );

        let steering = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: crate::RunId::new(1),
                    input: user_input(BlobRef::from_bytes(b"steer while parked")),
                },
                91,
            )
            .expect("steer parked run");
        commit_action(&mut drive, steering);
        // Parked: the await is not woken and nothing materializes yet.
        assert!(matches!(
            drive.next_action(92, 64).expect("next action"),
            CoreAgentAction::Idle
        ));
        let active_run = drive.state().runs.active.as_ref().expect("active");
        assert_eq!(active_run.status, RunStatus::Parked);
        assert_eq!(active_run.steering.len(), 1);
        assert!(active_run.steering[0].entry_ids.is_empty());

        let resumed = drive
            .admit_command(resume_tool_batch_command(&request), 93)
            .expect("resume await");
        commit_action(&mut drive, resumed);
        let next_request = drive_until_generate(&mut drive);
        let steering_entries = next_request
            .request
            .context
            .entries
            .iter()
            .filter(|entry| matches!(entry.source, ContextEntrySource::Steering { .. }))
            .count();
        assert_eq!(
            steering_entries, 1,
            "next turn must carry the steering entry"
        );
        let active_run = drive.state().runs.active.as_ref().expect("active");
        assert_eq!(
            active_run.steering[0].consumed_by_turn_id,
            Some(next_request.turn_id)
        );
    }

    #[test]
    fn steering_cancelling_run_is_rejected() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(&mut drive);
        let cancel = drive
            .admit_command(
                CoreAgentCommand::CancelRun {
                    run_id: request.run_id,
                },
                30,
            )
            .expect("cancel");
        commit_action(&mut drive, cancel);
        let error = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: crate::RunId::new(1),
                    input: user_input(BlobRef::from_bytes(b"too late")),
                },
                31,
            )
            .expect_err("steering a cancelling run is rejected");
        let CoreAgentDriveError::Command(CommandError::Rejected(rejection)) = error else {
            panic!("expected command rejection, got: {error:?}");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::ActiveWork);
    }

    /// Cancelling a run whose generation is in flight resolves the
    /// open turn as `cancelled` in the engine itself — no grace turn, no
    /// `failed` record, and the runtime is never asked for work again. A late
    /// generation result for the abandoned turn is rejected.
    #[test]
    fn cancel_during_generation_cancels_turn_and_drains_to_cancelled() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(&mut drive);

        let cancel = drive
            .admit_command(
                CoreAgentCommand::CancelRun {
                    run_id: request.run_id,
                },
                30,
            )
            .expect("cancel");
        commit_action(&mut drive, cancel);
        assert_eq!(
            drive.state().runs.active.as_ref().expect("active").status,
            RunStatus::Cancelling
        );

        let cancel_turn = drive.next_action(31, 64).expect("next action");
        let entries = commit_action(&mut drive, cancel_turn);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Turn(TurnEvent::Cancelled { turn_id, .. }) if turn_id == request.turn_id
        ));
        drain_to_idle(&mut drive, 32);

        assert!(drive.state().runs.active.is_none());
        let record = drive.state().runs.completed.last().expect("run record");
        assert_eq!(record.status, RunStatus::Cancelled);
        assert!(record.failure.is_none());

        let late = drive.resume_generation(
            LlmGenerationResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                status: LlmGenerationStatus::Succeeded,
                failure_ref: None,
                context_entries: Vec::new(),
                facts: LlmGenerationFacts {
                    duration_ms: None,
                    provider_response_id: Some("resp-late".to_owned()),
                    finish: LlmFinish::Stop,
                    usage: None,
                    tool_calls: Vec::new(),
                    approval_requests: Vec::new(),
                    context_token_estimate: None,
                },
            },
            33,
        );
        assert!(
            late.is_err(),
            "late result for a cancelled turn must be rejected"
        );
    }

    /// Cancelling a run while a tool batch executes records every
    /// non-terminal call as cancelled with the well-known content and drains
    /// the run to `cancelled` without runtime involvement.
    #[test]
    fn cancel_during_tool_batch_cancels_pending_calls_and_drains_to_cancelled() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);

        let cancel = drive
            .admit_command(
                CoreAgentCommand::CancelRun {
                    run_id: request.run_id,
                },
                90,
            )
            .expect("cancel");
        commit_action(&mut drive, cancel);

        let cancel_calls = drive.next_action(91, 64).expect("next action");
        let entries = commit_action(&mut drive, cancel_calls);
        let CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. }) = &entries[0].event
        else {
            panic!(
                "expected cancelled call completion, got {:?}",
                entries[0].event
            );
        };
        assert_eq!(result.call_id, request.calls[0].call_id);
        assert_eq!(result.status, ToolCallStatus::Cancelled);
        assert_eq!(result.error_ref, Some(crate::cancelled_tool_result_ref()));
        drain_to_idle(&mut drive, 92);

        assert!(drive.state().runs.active.is_none());
        let record = drive.state().runs.completed.last().expect("run record");
        assert_eq!(record.status, RunStatus::Cancelled);
        // The model-visible cancelled tool result is part of the context so
        // the next run's conversation stays well-formed.
        assert!(drive.state().context.entries.iter().any(|entry| matches!(
            &entry.kind,
            ContextEntryKind::ToolResult { call_id, is_error: true }
                if *call_id == request.calls[0].call_id
        )));

        let late = drive.resume_tool_batch(completed_tool_result(&request), 93);
        assert!(
            late.is_err(),
            "late result for a cancelled batch must be rejected"
        );
    }

    /// A cancel that lands after a tool-call turn completed but before
    /// its batch started still yields cancelled tool results for every call,
    /// so the conversation never carries tool calls without results.
    #[test]
    fn cancel_between_tool_call_turn_and_batch_start_records_cancelled_results() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        install_test_tool(&mut drive, "await");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let request = drive_until_generate(&mut drive);
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: Some("resp-tool".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: vec![ObservedToolCall {
                            call_id: crate::ToolCallId::new("call_wait"),
                            tool_id: Some(ToolName::new("concurrency.await")),
                            tool_name: ToolName::new("await"),
                            provider_kind: None,
                            arguments_ref: BlobRef::from_bytes(br#"{"wait":true}"#),
                            native_call_ref: None,
                        }],
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                80,
            )
            .expect("resume generation");
        commit_action(&mut drive, resumed);
        assert!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .expect("active")
                .active_tool_batch_id
                .is_none()
        );

        let cancel = drive
            .admit_command(
                CoreAgentCommand::CancelRun {
                    run_id: request.run_id,
                },
                81,
            )
            .expect("cancel");
        commit_action(&mut drive, cancel);
        drain_to_idle(&mut drive, 82);

        assert!(drive.state().runs.active.is_none());
        let record = drive.state().runs.completed.last().expect("run record");
        assert_eq!(record.status, RunStatus::Cancelled);
        assert!(drive.state().context.entries.iter().any(|entry| matches!(
            &entry.kind,
            ContextEntryKind::ToolResult { call_id, is_error: true }
                if call_id.as_str() == "call_wait"
        )));
    }

    /// Steering admitted during a run's final turn is not lost: the run
    /// gets one more turn whose request carries the steering, and only then
    /// completes.
    #[test]
    fn unconsumed_steering_extends_run_by_one_turn_after_final_output() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let first = drive_until_generate(&mut drive);

        let steering = drive
            .admit_command(
                CoreAgentCommand::RequestRunSteering {
                    run_id: crate::RunId::new(1),
                    input: user_input(BlobRef::from_bytes(b"late steering")),
                },
                30,
            )
            .expect("steer");
        commit_action(&mut drive, steering);

        let final_output = |turn: &LlmGenerationRequest, text: &[u8]| LlmGenerationResult {
            run_id: turn.run_id,
            turn_id: turn.turn_id,
            status: LlmGenerationStatus::Succeeded,
            failure_ref: None,
            context_entries: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant,
                },
                content: crate::ContentRef::text(BlobRef::from_bytes(text)),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }],
            facts: LlmGenerationFacts {
                duration_ms: None,
                provider_response_id: None,
                finish: LlmFinish::Stop,
                usage: None,
                tool_calls: Vec::new(),
                approval_requests: Vec::new(),
                context_token_estimate: None,
            },
        };
        let completed = drive
            .resume_generation(final_output(&first, b"first answer"), 31)
            .expect("first final");
        commit_action(&mut drive, completed);

        // Not terminal: a second turn is planned and its request carries
        // the steering entry.
        let second = drive_until_generate(&mut drive);
        assert_eq!(second.run_id, first.run_id);
        assert_ne!(second.turn_id, first.turn_id);
        assert_eq!(
            second
                .request
                .context
                .entries
                .iter()
                .filter(|entry| matches!(entry.source, ContextEntrySource::Steering { .. }))
                .count(),
            1
        );
        let active = drive.state().runs.active.as_ref().expect("active");
        assert_eq!(active.steering[0].consumed_by_turn_id, Some(second.turn_id));

        let completed = drive
            .resume_generation(final_output(&second, b"second answer"), 32)
            .expect("second final");
        commit_action(&mut drive, completed);
        drain_to_idle(&mut drive, 33);
        assert!(drive.state().runs.active.is_none());
        assert_eq!(
            drive.state().runs.completed.last().expect("record").status,
            RunStatus::Completed
        );
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
    fn replace_session_config_preserves_managed_workflow_tool_bindings() {
        let session_id = SessionId::new("session-managed");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let receiver = WorkflowEndpointRef {
            workflow_id: "work/controller-1".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        };
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: uuid::Uuid::from_u128(7),
                    workflow_tools: crate::ManagedSessionWorkflowTools::v1(
                        Some(receiver.clone()),
                        vec![crate::WorkflowToolDeclaration::bound_notify(
                            WorkflowToolDefinition {
                                tool_id: WorkflowToolId::new("work-report"),
                                revision: 1,
                                semantic_type: "lightspeed.work.report.v1".to_owned(),
                                tool: test_tool_spec("work_report"),
                            },
                            receiver,
                        )],
                    ),
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, open);
        let managed_bindings = drive.state().workflow_tools.clone();

        let mut next = config();
        next.generation.max_output_tokens = Some(2048);
        let replace = drive
            .admit_command(
                CoreAgentCommand::ReplaceSessionConfig {
                    expected_revision: Some(0),
                    config: next,
                },
                20,
            )
            .expect("replace public session config");
        commit_action(&mut drive, replace);

        assert_eq!(drive.state().lifecycle.config_revision, 1);
        assert_eq!(drive.state().workflow_tools, managed_bindings);
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
    fn unbounded_drive_ignores_the_explicit_runner_step_counter() {
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

        drive.steps_taken = 128;
        assert_eq!(
            drive.next_action(21, 128).expect("bounded next action"),
            CoreAgentAction::StepLimitReached
        );
        assert!(matches!(
            drive
                .next_action_unbounded(21)
                .expect("unbounded next action"),
            CoreAgentAction::AppendEvents { .. }
        ));
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
                        content: crate::ContentRef {
                            content_ref: BlobRef::from_bytes(b"assistant output"),
                            media_type: None,
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
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

    /// A provider content filter (an Anthropic `refusal`, an OpenAI
    /// `content_filter` stop) fails the run like a model failure instead of
    /// completing it with an empty answer; the adapter's failure text rides
    /// along as the run failure message.
    /// An output-cap cut-off fails the run like a model failure, but the
    /// partial text the adapter kept is still applied to the context so the
    /// user sees what was produced before the cut.
    #[test]
    fn length_finish_fails_run_but_keeps_partial_output() {
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
        let partial_ref = BlobRef::from_bytes(b"The bicycle was");
        let failure_ref = BlobRef::from_bytes(b"cut off at max output tokens 48");
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: llm_request.run_id,
                    turn_id: llm_request.turn_id,
                    status: LlmGenerationStatus::Failed,
                    failure_ref: Some(failure_ref.clone()),
                    context_entries: vec![message_input(
                        ContextMessageRole::Assistant,
                        partial_ref.clone(),
                    )],
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: Some("msg_cut".to_owned()),
                        finish: LlmFinish::Length,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .expect("resume truncated generation");
        commit_action(&mut drive, resumed);

        let fail_run = drive.next_action(31, 8).expect("fail run");
        let entries = commit_action(&mut drive, fail_run);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Run(crate::RunEvent::Failed { .. })
        ));
        let completed = drive.state().runs.completed.last().expect("completed run");
        assert_eq!(completed.status, RunStatus::Failed);
        let failure = completed.failure.as_ref().expect("run failure");
        assert_eq!(failure.kind, RunFailureKind::ModelFailure);
        assert_eq!(failure.message_ref.as_ref(), Some(&failure_ref));
        assert!(
            drive
                .state()
                .context
                .entries
                .iter()
                .any(|entry| entry.content.content_ref == partial_ref),
            "the partial text must stay in the active context: {:?}",
            drive.state().context.entries
        );
        assert!(matches!(
            drive.next_action(32, 8).expect("next"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn content_filter_finish_fails_run_like_a_model_failure() {
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
        let failure_ref = BlobRef::from_bytes(b"provider refused (category: cyber)");
        let resumed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: llm_request.run_id,
                    turn_id: llm_request.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: Some(failure_ref.clone()),
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: Some("msg_refused".to_owned()),
                        finish: LlmFinish::ContentFilter,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .expect("resume content-filtered generation");
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
                        duration_ms: None,
                        provider_response_id: None,
                        finish: LlmFinish::Failed,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
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

        let parked = park_on_wait_promise(&mut drive, &request);
        let deferred = drive
            .resume_tool_batch_outcome(parked, 90)
            .expect("defer tool batch");
        let entries = commit_action(&mut drive, deferred);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Tool(ToolEvent::BatchDeferred { .. })
        ));

        let active_run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(
            active_run
                .parked_tool_batch
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
        let promise_id = crate::PromiseId::new("promise_1");
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Timer { fire_at_ms: 1 },
                scope: crate::PromiseScope::Run {
                    run_id: request.run_id,
                },
                ownership: crate::PromiseOwnership::Model,
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
                    },
                ),
                90,
            )
            .expect("defer resolved await");
        commit_action(&mut drive, deferred);

        assert_eq!(await_wake(drive.state(), 91), Some(WakeReason::Terminal));
        let resumed = drive
            .admit_command(
                resume_tool_batch_command_with_claim(&request, WakeReason::Terminal),
                91,
            )
            .expect("resume terminal await");
        let entries = commit_action(&mut drive, resumed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Tool(ToolEvent::BatchResumed { .. })
        )));
        let result_ref = BlobRef::from_bytes(b"await output");
        let result = entries
            .iter()
            .find_map(|entry| match &entry.event {
                CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. }) => Some(result),
                _ => None,
            })
            .expect("await call completion");
        assert_eq!(result.output_ref.as_ref(), Some(&result_ref));
        assert_eq!(result.model_visible_context_entries.len(), 1);
        assert!(matches!(
            result.model_visible_context_entries[0].kind,
            ContextEntryKind::ToolResult { .. }
        ));
        assert_eq!(
            result.model_visible_context_entries[0].content.content_ref,
            result_ref
        );
        assert!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .expect("active run")
                .parked_tool_batch
                .is_none()
        );
    }

    #[test]
    fn zero_timeout_await_parks_then_wakes_timeout() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let promise_id = crate::PromiseId::new("promise_1");
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Timer { fire_at_ms: 1_000 },
                scope: crate::PromiseScope::Run {
                    run_id: request.run_id,
                },
                ownership: crate::PromiseOwnership::Model,
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
                        promise_ids: vec![crate::PromiseId::new("promise_99")],
                        mode: AwaitMode::All,
                        deadline_at_ms: None,
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
        assert!(active_run.parked_tool_batch.is_none());
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
        let parked = park_on_wait_promise(&mut drive, &request);
        let deferred = drive
            .resume_tool_batch_outcome(parked, 90)
            .expect("defer tool batch");
        commit_action(&mut drive, deferred);

        let resumed = drive
            .admit_command(resume_tool_batch_command(&request), 91)
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
        assert!(active_run.parked_tool_batch.is_none());
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);

        let duplicate = drive
            .admit_command(resume_tool_batch_command(&request), 92)
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
        assert!(!active_run.tool_batches.contains_key(&request.batch_id));
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
        assert!(active_run.parked_tool_batch.is_none());
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
                ) && entry.content.content_ref == extra_ref
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
                && entry.content.content_ref == tool_result_ref
        }));
        assert!(drive.state().context.entries.iter().any(|entry| {
            matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User
                }
            ) && entry.content.content_ref == extra_ref
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
    fn duplicate_submission_with_different_terminal_notification_is_rejected() {
        let mut drive =
            CoreAgentDrive::from_replayed(SessionId::new("session-a"), CoreAgentState::new(), None);
        open_session(&mut drive);
        let command = |token: &str| {
            let mut command = request_run_command(
                Some(crate::SubmissionId::new("retry_notify")),
                user_input(BlobRef::from_bytes(b"x")),
                run_config(),
            );
            let CoreAgentCommand::RequestRun(request) = &mut command else {
                unreachable!("request_run_command always constructs RequestRun");
            };
            request.notify_on_terminal = vec![crate::RunTerminalNotifyIntent {
                holder_workflow_id: "controller-1".to_owned(),
                token: token.to_owned(),
            }];
            command
        };

        let accepted = drive
            .admit_command(command("token-1"), 20)
            .expect("first request run");
        commit_action(&mut drive, accepted);
        let duplicate = drive
            .admit_command(command("token-1"), 21)
            .expect("identical notification retry");
        assert!(!matches!(duplicate, CoreAgentAction::AppendEvents { .. }));

        let error = drive
            .admit_command(command("token-2"), 22)
            .expect_err("different terminal token must fail");
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
                        duration_ms: None,
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
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
        let parked = park_on_wait_promise(&mut drive, &request);
        let deferred = drive
            .resume_tool_batch_outcome(parked, 90)
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
                resume_tool_batch_command_with_claim(&request, WakeReason::Cancelled),
                92,
            )
            .expect("resume while cancelling");
        commit_action(&mut drive, resumed);

        // No grace turn: once the resumed batch has drained, the run
        // is cancelled without another model call (`drain_to_idle` panics on
        // any `GenerateLlm` action).
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
        let parked = park_on_wait_promise(&mut drive, &request);
        let deferred = drive
            .resume_tool_batch_outcome(parked, 90)
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
        let parked = park_on_wait_promise(&mut drive, &request);
        let deferred = drive
            .resume_tool_batch_outcome(parked, 90)
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
        // Force-cancelling the parked run cascades to its run-scoped wait
        // promise; the order is force-cancel, queued-cancel, closed.
        let position = |predicate: fn(&CoreAgentEvent) -> bool| {
            entries
                .iter()
                .position(|entry| predicate(&entry.event))
                .expect("event present")
        };
        let force_cancelled = position(|event| {
            matches!(
                event,
                CoreAgentEvent::Run(crate::RunEvent::ForceCancelled { .. })
            )
        });
        let queued_cancelled = position(|event| {
            matches!(
                event,
                CoreAgentEvent::Run(crate::RunEvent::QueuedCancelled { .. })
            )
        });
        let closed = position(|event| {
            matches!(
                event,
                CoreAgentEvent::Lifecycle(crate::CoreAgentLifecycleEvent::Closed)
            )
        });
        assert!(force_cancelled < queued_cancelled && queued_cancelled < closed);
        assert!(entries.iter().any(|entry| matches!(
            entry.event,
            CoreAgentEvent::Promise(crate::PromiseEvent::Cancelled { .. })
        )));
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
            &crate::PromiseSource::Timer {
                fire_at_ms: u64::MAX,
            },
            None,
        )];
        result
    }

    #[test]
    fn pushed_accepted_workflow_tool_stays_successful_after_delivery_failure() {
        let session_id = SessionId::new("session-tool");
        let universe_id = uuid::Uuid::from_u128(7);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("report"),
            revision: 1,
            semantic_type: "lightspeed.work.report.v1".to_owned(),
            tool: test_tool_spec("work_report"),
        };
        let controller = WorkflowEndpointRef {
            workflow_id: "opaque work workflow id".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        };
        let declaration = crate::ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![crate::WorkflowToolDeclaration::new(
                definition.clone(),
                crate::WorkflowToolTarget::Bound {
                    receiver: controller,
                    dispatch: crate::BoundWorkflowToolDispatch::Push,
                },
                crate::WorkflowToolCompletion::Accepted,
            )],
        );
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: universe_id,
                    workflow_tools: declaration,
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
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
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
            tool_id: definition.tool_id,
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
            execution_context_ref: None,
            completion_promises: None,
        };
        let mut result = completed_tool_result(&request);
        result.results[0].effects = vec![crate::workflow_tool_emit_effect(&invocation)];

        let resumed = drive
            .resume_tool_batch(result, 90)
            .expect("resume workflow tool");
        let CoreAgentAction::AppendEvents { events, .. } = &resumed else {
            panic!("expected append");
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.kind, "lightspeed.core.tool.call_completed");
        assert_eq!(
            events[1].event.kind,
            "lightspeed.core.workflow_tool.emitted"
        );

        commit_action(&mut drive, resumed);
        assert_eq!(
            drive.state().workflow_tools.emissions.get(&invocation_id),
            Some(&invocation)
        );
        assert!(drive.state().promises.promises.is_empty());

        let failed = drive
            .admit_command(
                CoreAgentCommand::FailWorkflowToolDelivery {
                    invocation_id: invocation_id.clone(),
                    error_ref: BlobRef::from_bytes(b"receiver unreachable"),
                },
                95,
            )
            .expect("record pushed Accepted delivery failure");
        let entries = commit_action(&mut drive, failed);
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::WorkflowTool(crate::WorkflowToolEvent::DeliveryFailed { .. })
        ));
        let active = drive.state().runs.active.as_ref().expect("active run");
        let batch = active
            .tool_batches
            .get(&request.batch_id)
            .expect("active tool batch");
        let call = batch
            .calls
            .iter()
            .find(|state| state.call.call_id == invocation.tool_call_id)
            .expect("workflow tool call");
        assert_eq!(call.status, ToolCallStatus::Succeeded);
        assert!(call.result.as_ref().is_some_and(|result| {
            result.status == ToolCallStatus::Succeeded && result.error_ref.is_none()
        }));
    }

    #[test]
    fn joined_workflow_tool_parks_and_resumes_the_original_call() {
        let session_id = SessionId::new("session-tool");
        let universe_id = uuid::Uuid::from_u128(7);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("send"),
            revision: 1,
            semantic_type: "lightspeed.message.receipt.v1".to_owned(),
            tool: test_tool_spec("message_send"),
        };
        let declaration = crate::ManagedSessionWorkflowTools::v1(
            None,
            vec![crate::WorkflowToolDeclaration::new(
                definition.clone(),
                crate::WorkflowToolTarget::Bound {
                    receiver: WorkflowEndpointRef {
                        workflow_id: "channels controller".to_owned(),
                        workflow_kind: "channels.session".to_owned(),
                    },
                    dispatch: crate::BoundWorkflowToolDispatch::Push,
                },
                crate::WorkflowToolCompletion::Joined {
                    reply_schema_ref: None,
                    deadline_after_ms: 60_000,
                },
            )],
        );
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: universe_id,
                    workflow_tools: declaration,
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, open);
        install_test_tool(&mut drive, "message_send");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let request = drive_until_tool_batch_request(&mut drive, generation, "message_send");
        let binding = drive
            .state()
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
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
        let promise_id = crate::PromiseId::from_number(1);
        let invocation = WorkflowToolInvocation {
            invocation_id: invocation_id.clone(),
            tool_id: definition.tool_id,
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
            execution_context_ref: None,
            completion_promises: Some(BTreeMap::from([(
                crate::REPLY_COMPLETION_KEY.to_owned(),
                promise_id.clone(),
            )])),
        };
        let mut result = completed_tool_result(&request);
        result.results[0].effects = vec![crate::with_completion_deadline(
            crate::workflow_tool_emit_effect(&invocation),
            Some(60_090),
        )];

        let parked = drive
            .resume_tool_batch(result, 90)
            .expect("admit Joined workflow tool");
        let CoreAgentAction::AppendEvents { events, .. } = &parked else {
            panic!("expected joined admission append");
        };
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event.kind, "lightspeed.core.promise.created");
        assert_eq!(events[1].event.kind, "lightspeed.core.tool.batch_deferred");
        assert_eq!(
            events[2].event.kind,
            "lightspeed.core.workflow_tool.emitted"
        );
        assert!(
            !events
                .iter()
                .any(|event| { event.event.kind == "lightspeed.core.tool.call_completed" })
        );
        commit_action(&mut drive, parked);

        let active = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(active.status, RunStatus::Parked);
        let suspension = active.parked_tool_batch.as_ref().expect("parked batch");
        assert!(matches!(
            &suspension.suspension,
            ToolBatchSuspension::JoinedWorkflowCalls { calls, spec }
                if calls.len() == 1
                    && calls[0].call_id == call.call_id
                    && calls[0].invocation_id == invocation_id
                    && calls[0].promise_id == promise_id
                    && spec.promise_ids == vec![promise_id.clone()]
        ));
        let promise = drive
            .state()
            .promises
            .promises
            .get(&promise_id)
            .expect("internal reply Promise");
        assert_eq!(promise.ownership, PromiseOwnership::Runtime);
        assert_eq!(promise.deadline_ms, Some(60_090));
        assert!(
            validate_await_spec_for_active_run(
                drive.state(),
                request.run_id,
                &AwaitSpec {
                    promise_ids: vec![promise_id.clone()],
                    mode: AwaitMode::All,
                    deadline_at_ms: None,
                }
            )
            .is_err()
        );
        for effect in [
            crate::promise_cancel_effect(&promise_id),
            crate::promise_detach_effect(&promise_id),
        ] {
            let forbidden = ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results: vec![ToolInvocationResult {
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: None,
                    model_visible_context_entries: Vec::new(),
                    error_ref: None,
                    effects: vec![effect],
                }],
            };
            let error =
                tool_call_completed_proposals(drive.state(), Some(drive.session_id()), forbidden)
                    .expect_err("runtime-owned Promise rejects model control");
            assert!(error.to_string().contains("runtime-owned"));
        }
        for (promise_status, expected_call_status) in [
            (PromiseStatus::Failed, ToolCallStatus::Failed),
            (PromiseStatus::Cancelled, ToolCallStatus::Cancelled),
        ] {
            let mut terminal_state = drive.state().clone();
            let promise = terminal_state
                .promises
                .promises
                .get_mut(&promise_id)
                .expect("internal reply Promise");
            promise.status = promise_status;
            promise.error_ref = (promise_status == PromiseStatus::Failed)
                .then(|| BlobRef::from_bytes(b"reply failed"));
            let resumed = joined_workflow_resume_result(&terminal_state, false, &[])
                .expect("map terminal Joined Promise");
            assert_eq!(resumed.results[0].call_id, call.call_id);
            assert_eq!(resumed.results[0].status, expected_call_status);
        }
        let mut cancelling_state = drive.state().clone();
        cancelling_state
            .runs
            .active
            .as_mut()
            .expect("active run")
            .status = RunStatus::Cancelling;
        let cancellation = resume_tool_batch_proposals(
            &cancelling_state,
            ResumeToolBatchCommand {
                run_id: request.run_id,
                batch_id: request.batch_id,
                claim: WakeReason::Cancelled,
                claim_observed_at_ms: 100,
                output: ToolBatchResumeOutput::JoinedWorkflowCalls {
                    additional_context: Vec::new(),
                },
            },
            100,
        )
        .expect("cancel parked Joined batch");
        assert!(matches!(
            cancellation[0].event,
            CoreAgentEvent::Promise(PromiseEvent::Cancelled { .. })
        ));
        assert!(cancellation.iter().any(|proposal| matches!(
            &proposal.event,
            CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. })
                if result.status == ToolCallStatus::Cancelled
        )));

        let payload_ref = BlobRef::from_bytes(br#"{"receipt":"sent"}"#);
        let resolved = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: promise_id.clone(),
                    resolution: crate::PromiseResolution::Resolved {
                        payload_ref: Some(payload_ref.clone()),
                    },
                },
                100,
            )
            .expect("resolve Joined reply");
        commit_action(&mut drive, resolved);
        assert_eq!(await_wake(drive.state(), 100), Some(WakeReason::Terminal));

        // The payload named one image; it follows the result as a media entry.
        let handed_over = crate::media::MediaDescriptor::new(
            BlobRef::from_bytes(b"png bytes"),
            "image/png",
            Some("render.png"),
        )
        .expect("admitted descriptor");
        let resumed = drive
            .admit_command(
                CoreAgentCommand::ResumeToolBatch(ResumeToolBatchCommand {
                    run_id: request.run_id,
                    batch_id: request.batch_id,
                    claim: WakeReason::Terminal,
                    claim_observed_at_ms: 100,
                    output: ToolBatchResumeOutput::JoinedWorkflowCalls {
                        additional_context: vec![crate::PromiseContextEntries {
                            promise_id: promise_id.clone(),
                            entries: vec![handed_over.context_entry()],
                        }],
                    },
                }),
                100,
            )
            .expect("resume Joined batch");
        let entries = commit_action(&mut drive, resumed);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::Tool(ToolEvent::BatchResumed { .. })
        ));
        let CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. }) = &entries[1].event
        else {
            panic!("expected original call completion");
        };
        assert_eq!(result.call_id, call.call_id);
        assert_eq!(result.status, ToolCallStatus::Succeeded);
        assert_eq!(result.output_ref.as_ref(), Some(&payload_ref));
        assert_eq!(result.model_visible_context_entries.len(), 2);
        let media_entry = &result.model_visible_context_entries[1];
        assert!(matches!(
            media_entry.kind,
            ContextEntryKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(media_entry.content, handed_over.context_entry().content);
        assert_eq!(media_entry.preview.as_deref(), Some("[image: render.png]"));
        assert!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .is_some_and(|run| run.parked_tool_batch.is_none())
        );
    }

    #[test]
    fn multiple_joined_calls_park_all_of_and_preserve_completed_ordinary_calls() {
        let session_id = SessionId::new("session-tool");
        let universe_id = uuid::Uuid::from_u128(8);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let workflow_spec = test_tool_spec("message_send");
        let ordinary_spec = test_tool_spec("local_echo");
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("send"),
            revision: 1,
            semantic_type: "lightspeed.message.receipt.v1".to_owned(),
            tool: workflow_spec.clone(),
        };
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: universe_id,
                    workflow_tools: crate::ManagedSessionWorkflowTools::v1(
                        None,
                        vec![crate::WorkflowToolDeclaration::new(
                            definition.clone(),
                            crate::WorkflowToolTarget::Bound {
                                receiver: WorkflowEndpointRef {
                                    workflow_id: "channels controller".to_owned(),
                                    workflow_kind: "channels.session".to_owned(),
                                },
                                dispatch: crate::BoundWorkflowToolDispatch::Push,
                            },
                            crate::WorkflowToolCompletion::Joined {
                                reply_schema_ref: None,
                                deadline_after_ms: 60_000,
                            },
                        )],
                    ),
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, open);
        let tools = BTreeMap::from([
            (workflow_spec.name.clone(), workflow_spec),
            (ordinary_spec.name.clone(), ordinary_spec),
        ]);
        let installed = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(drive.state().tooling.revision),
                    tools,
                },
                15,
            )
            .expect("install mixed toolset");
        commit_action(&mut drive, installed);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let observed_calls = vec![
            ObservedToolCall {
                call_id: ToolCallId::new("call-send-1"),
                tool_id: Some(ToolName::new("message_send")),
                tool_name: ToolName::new("message_send"),
                provider_kind: None,
                arguments_ref: BlobRef::from_bytes(br#"{"text":"one"}"#),
                native_call_ref: None,
            },
            ObservedToolCall {
                call_id: ToolCallId::new("call-echo"),
                tool_id: Some(ToolName::new("local_echo")),
                tool_name: ToolName::new("local_echo"),
                provider_kind: None,
                arguments_ref: BlobRef::from_bytes(br#"{"text":"echo"}"#),
                native_call_ref: None,
            },
            ObservedToolCall {
                call_id: ToolCallId::new("call-send-2"),
                tool_id: Some(ToolName::new("message_send")),
                tool_name: ToolName::new("message_send"),
                provider_kind: None,
                arguments_ref: BlobRef::from_bytes(br#"{"text":"two"}"#),
                native_call_ref: None,
            },
        ];
        let request =
            drive_until_tool_batch_request_with_calls(&mut drive, generation, observed_calls);
        let binding = drive
            .state()
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
            .cloned()
            .expect("binding");
        let promise_ids = crate::PromiseIdAllocator::new(request.promise_id_base);
        let mut joined_ids = Vec::new();
        let mut results = Vec::new();
        for call in &request.calls {
            if call.tool_name.as_str() == "local_echo" {
                let content_ref = BlobRef::from_bytes(b"echo complete");
                results.push(ToolInvocationResult {
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(content_ref.clone()),
                    model_visible_context_entries: vec![
                        ToolInvocationResult::tool_result_context_entry(
                            &call.call_id,
                            ToolCallStatus::Succeeded,
                            content_ref,
                        ),
                    ],
                    error_ref: None,
                    effects: Vec::new(),
                });
                continue;
            }
            let invocation_id = crate::WorkflowToolInvocationId::for_call(
                universe_id,
                &session_id,
                request.run_id,
                request.turn_id,
                request.batch_id,
                &call.call_id,
                &binding.binding_fingerprint,
            );
            let promise_id = promise_ids.allocate();
            let invocation = WorkflowToolInvocation {
                invocation_id: invocation_id.clone(),
                tool_id: definition.tool_id.clone(),
                semantic_type: definition.semantic_type.clone(),
                schema_revision: definition.revision,
                binding_fingerprint: binding.binding_fingerprint.clone(),
                session_universe_id: universe_id,
                session_id: session_id.clone(),
                run_id: request.run_id,
                turn_id: request.turn_id,
                tool_batch_id: request.batch_id,
                tool_call_id: call.call_id.clone(),
                arguments_ref: call.arguments_ref.clone(),
                execution_context_ref: None,
                completion_promises: Some(BTreeMap::from([(
                    crate::REPLY_COMPLETION_KEY.to_owned(),
                    promise_id.clone(),
                )])),
            };
            joined_ids.push((call.call_id.clone(), promise_id));
            results.push(ToolInvocationResult {
                duration_ms: None,
                output_bytes: None,
                truncated: false,
                call_id: call.call_id.clone(),
                status: ToolCallStatus::Succeeded,
                output_ref: None,
                model_visible_context_entries: Vec::new(),
                error_ref: None,
                effects: vec![crate::with_completion_deadline(
                    crate::workflow_tool_emit_effect(&invocation),
                    Some(60_090),
                )],
            });
        }
        let parked = drive
            .resume_tool_batch(
                ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results,
                },
                90,
            )
            .expect("park mixed batch");
        commit_action(&mut drive, parked);

        let active = drive.state().runs.active.as_ref().expect("active run");
        let batch = active
            .tool_batches
            .get(&request.batch_id)
            .expect("active batch");
        let echo = batch
            .calls
            .iter()
            .find(|call| call.call.call_id.as_str() == "call-echo")
            .expect("ordinary call");
        assert_eq!(echo.status, ToolCallStatus::Succeeded);
        let ToolBatchSuspension::JoinedWorkflowCalls { calls, spec } = &active
            .parked_tool_batch
            .as_ref()
            .expect("parked batch")
            .suspension
        else {
            panic!("expected joined suspension");
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(spec.mode, AwaitMode::All);
        assert_eq!(spec.promise_ids.len(), 2);

        for (_, promise_id) in &joined_ids {
            let resolved = drive
                .admit_command(
                    CoreAgentCommand::ResolvePromise {
                        promise_id: promise_id.clone(),
                        resolution: crate::PromiseResolution::Resolved {
                            payload_ref: Some(BlobRef::from_bytes(promise_id.as_str().as_bytes())),
                        },
                    },
                    100,
                )
                .expect("resolve Joined reply");
            commit_action(&mut drive, resolved);
        }
        let resumed = drive
            .admit_command(
                CoreAgentCommand::ResumeToolBatch(ResumeToolBatchCommand {
                    run_id: request.run_id,
                    batch_id: request.batch_id,
                    claim: WakeReason::Terminal,
                    claim_observed_at_ms: 100,
                    output: ToolBatchResumeOutput::JoinedWorkflowCalls {
                        additional_context: Vec::new(),
                    },
                }),
                100,
            )
            .expect("resume all Joined calls");
        let resumed_entries = commit_action(&mut drive, resumed);
        let completed_call_ids = resumed_entries
            .iter()
            .filter_map(|entry| match &entry.event {
                CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. }) => {
                    Some(result.call_id.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            completed_call_ids,
            joined_ids.into_iter().map(|(call_id, _)| call_id).collect()
        );
    }

    #[test]
    fn explicit_await_and_joined_calls_in_one_batch_fail_without_parking() {
        let session_id = SessionId::new("session-tool");
        let universe_id = uuid::Uuid::from_u128(9);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session(&mut drive);
        let workflow_spec = test_tool_spec("message_send");
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("send"),
            revision: 1,
            semantic_type: "lightspeed.message.receipt.v1".to_owned(),
            tool: workflow_spec.clone(),
        };
        let admitted = drive
            .admit_command(
                CoreAgentCommand::AdmitSystemWorkflowTool {
                    session_universe_id: universe_id,
                    declaration: crate::WorkflowToolDeclaration::new(
                        definition.clone(),
                        crate::WorkflowToolTarget::Bound {
                            receiver: WorkflowEndpointRef {
                                workflow_id: "channels controller".to_owned(),
                                workflow_kind: "channels.session".to_owned(),
                            },
                            dispatch: crate::BoundWorkflowToolDispatch::Push,
                        },
                        crate::WorkflowToolCompletion::Joined {
                            reply_schema_ref: None,
                            deadline_after_ms: 60_000,
                        },
                    ),
                },
                12,
            )
            .expect("admit Joined system tool");
        commit_action(&mut drive, admitted);
        let await_spec = test_tool_spec(AWAIT_TOOL_ID);
        let installed = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(drive.state().tooling.revision),
                    tools: BTreeMap::from([
                        (workflow_spec.name.clone(), workflow_spec),
                        (await_spec.name.clone(), await_spec),
                    ]),
                },
                15,
            )
            .expect("install workflow and await tools");
        commit_action(&mut drive, installed);
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let request = drive_until_tool_batch_request_with_calls(
            &mut drive,
            generation,
            vec![
                ObservedToolCall {
                    call_id: ToolCallId::new("call-send"),
                    tool_id: Some(ToolName::new("message_send")),
                    tool_name: ToolName::new("message_send"),
                    provider_kind: None,
                    arguments_ref: BlobRef::from_bytes(br#"{"text":"one"}"#),
                    native_call_ref: None,
                },
                ObservedToolCall {
                    call_id: ToolCallId::new("call-await"),
                    tool_id: Some(ToolName::new(AWAIT_TOOL_ID)),
                    tool_name: ToolName::new("await"),
                    provider_kind: None,
                    arguments_ref: BlobRef::from_bytes(br#"{"promises":["other"]}"#),
                    native_call_ref: None,
                },
            ],
        );
        let binding = drive
            .state()
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
            .cloned()
            .expect("binding");
        let workflow_call = request
            .calls
            .iter()
            .find(|call| call.tool_name.as_str() == "message_send")
            .expect("workflow call");
        let await_call = request
            .calls
            .iter()
            .find(|call| {
                call.tool_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == AWAIT_TOOL_ID)
            })
            .expect("await call");
        let invocation_id = crate::WorkflowToolInvocationId::for_call(
            universe_id,
            &session_id,
            request.run_id,
            request.turn_id,
            request.batch_id,
            &workflow_call.call_id,
            &binding.binding_fingerprint,
        );
        let promise_id = crate::PromiseId::from_number(1);
        let invocation = WorkflowToolInvocation {
            invocation_id,
            tool_id: definition.tool_id,
            semantic_type: definition.semantic_type,
            schema_revision: definition.revision,
            binding_fingerprint: binding.binding_fingerprint,
            session_universe_id: universe_id,
            session_id,
            run_id: request.run_id,
            turn_id: request.turn_id,
            tool_batch_id: request.batch_id,
            tool_call_id: workflow_call.call_id.clone(),
            arguments_ref: workflow_call.arguments_ref.clone(),
            execution_context_ref: None,
            completion_promises: Some(BTreeMap::from([(
                crate::REPLY_COMPLETION_KEY.to_owned(),
                promise_id,
            )])),
        };
        let completed_results = vec![ToolInvocationResult {
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: workflow_call.call_id.clone(),
            status: ToolCallStatus::Succeeded,
            output_ref: None,
            model_visible_context_entries: Vec::new(),
            error_ref: None,
            effects: vec![crate::with_completion_deadline(
                crate::workflow_tool_emit_effect(&invocation),
                Some(60_090),
            )],
        }];
        let rejected = drive
            .defer_tool_batch(
                request.batch_id,
                await_call.call_id.clone(),
                completed_results,
                AwaitSpec {
                    promise_ids: Vec::new(),
                    mode: AwaitMode::All,
                    deadline_at_ms: Some(1_000),
                },
                90,
            )
            .expect("deterministically fail incompatible suspension calls");
        let entries = commit_action(&mut drive, rejected);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| matches!(
            &entry.event,
            CoreAgentEvent::Tool(ToolEvent::CallCompleted { result, .. })
                if result.status == ToolCallStatus::Failed
        )));
        assert!(drive.state().workflow_tools.emissions.is_empty());
        assert!(drive.state().promises.promises.is_empty());
        assert!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .is_some_and(|run| run.parked_tool_batch.is_none())
        );
    }

    #[test]
    fn promise_bearing_workflow_tool_creates_keyed_promises_atomically() {
        let session_id = SessionId::new("session-tool");
        let universe_id = uuid::Uuid::from_u128(7);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("approve"),
            revision: 1,
            semantic_type: "lightspeed.approval.request.v1".to_owned(),
            tool: test_tool_spec("request_approval"),
        };
        let controller = WorkflowEndpointRef {
            workflow_id: "opaque work workflow id".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        };
        let receiver = WorkflowEndpointRef {
            workflow_id: "approval plugin workflow id".to_owned(),
            workflow_kind: "approvals".to_owned(),
        };
        let declaration = crate::ManagedSessionWorkflowTools::v1(
            Some(controller),
            vec![crate::WorkflowToolDeclaration::new(
                definition.clone(),
                crate::WorkflowToolTarget::Bound {
                    receiver: receiver.clone(),
                    dispatch: crate::BoundWorkflowToolDispatch::Push,
                },
                crate::WorkflowToolCompletion::Promises {
                    reply_schema_ref: None,
                    deadline_after_ms: Some(60_000),
                    max_promises: 1,
                    key_source: crate::WorkflowToolCompletionKeySource::Reply,
                },
            )],
        );
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: universe_id,
                    workflow_tools: declaration,
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, open);
        install_test_tool(&mut drive, "request_approval");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let request = drive_until_tool_batch_request(&mut drive, generation, "request_approval");
        let binding = drive
            .state()
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
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
        let promise_id = crate::PromiseId::from_number(1);
        let invocation = WorkflowToolInvocation {
            invocation_id: invocation_id.clone(),
            tool_id: definition.tool_id,
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
            execution_context_ref: None,
            completion_promises: Some(std::collections::BTreeMap::from([(
                crate::REPLY_COMPLETION_KEY.to_owned(),
                promise_id.clone(),
            )])),
        };
        let mut result = completed_tool_result(&request);
        result.results[0].effects = vec![crate::with_completion_deadline(
            crate::workflow_tool_emit_effect(&invocation),
            Some(90_060_000),
        )];

        let resumed = drive
            .resume_tool_batch(result, 90)
            .expect("resume promise-bearing workflow tool");
        let CoreAgentAction::AppendEvents { events, .. } = &resumed else {
            panic!("expected append");
        };
        // One append: tool completion, keyed promise, then the emitted fact
        // whose apply verifies the promise exists with the canonical source.
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event.kind, "lightspeed.core.tool.call_completed");
        assert_eq!(events[1].event.kind, "lightspeed.core.promise.created");
        assert_eq!(
            events[2].event.kind,
            "lightspeed.core.workflow_tool.emitted"
        );
        commit_action(&mut drive, resumed);

        let promise = drive
            .state()
            .promises
            .promises
            .get(&promise_id)
            .expect("keyed completion promise");
        assert_eq!(promise.status, crate::PromiseStatus::Pending);
        assert_eq!(
            promise.scope,
            crate::PromiseScope::Run {
                run_id: request.run_id
            }
        );
        assert_eq!(promise.deadline_ms, Some(90_060_000));
        assert_eq!(
            promise.source,
            crate::PromiseSource::Workflow {
                producer_workflow_id: receiver.workflow_id.clone(),
                producer_workflow_kind: receiver.workflow_kind.clone(),
                invocation_id: invocation_id.as_str().to_owned(),
                completion_key: crate::REPLY_COMPLETION_KEY.to_owned(),
            }
        );

        // Terminal delivery failure fails the still-pending keyed promise
        // atomically with the DeliveryFailed fact.
        let error_ref = BlobRef::from_bytes(b"receiver unreachable");
        let failed = drive
            .admit_command(
                CoreAgentCommand::FailWorkflowToolDelivery {
                    invocation_id: invocation_id.clone(),
                    error_ref: error_ref.clone(),
                },
                95,
            )
            .expect("terminal delivery failure");
        let entries = commit_action(&mut drive, failed);
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::WorkflowTool(crate::WorkflowToolEvent::DeliveryFailed { .. })
        ));
        assert!(matches!(
            entries[1].event,
            CoreAgentEvent::Promise(crate::PromiseEvent::Failed { .. })
        ));
        let promise = drive
            .state()
            .promises
            .promises
            .get(&promise_id)
            .expect("failed promise");
        assert_eq!(promise.status, crate::PromiseStatus::Failed);
        assert_eq!(promise.error_ref, Some(error_ref.clone()));

        // Retried admission with the same error is an idempotent no-op.
        let retry = drive
            .admit_command(
                CoreAgentCommand::FailWorkflowToolDelivery {
                    invocation_id,
                    error_ref,
                },
                96,
            )
            .expect("idempotent retry");
        assert!(
            !matches!(retry, CoreAgentAction::AppendEvents { .. }),
            "delivery-failure retry must be a no-op: {retry:?}"
        );
    }

    #[test]
    fn start_on_call_tool_records_intent_and_keyed_promises_atomically() {
        let session_id = SessionId::new("session-tool");
        let universe_id = uuid::Uuid::from_u128(7);
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let definition = WorkflowToolDefinition {
            tool_id: WorkflowToolId::new("launch"),
            revision: 1,
            semantic_type: "lightspeed.job.launch.v1".to_owned(),
            tool: test_tool_spec("launch_job"),
        };
        let start = crate::WorkflowStartRef {
            recipe_format: 1,
            revision: 1,
            recipe_ref: BlobRef::from_bytes(b"{\"workflowType\":\"t\",\"taskQueue\":\"q\"}"),
            recipe_fingerprint: "wtr:sha256:recipe".to_owned(),
        };
        let declaration = crate::ManagedSessionWorkflowTools::v1(
            None,
            vec![crate::WorkflowToolDeclaration::new(
                definition.clone(),
                crate::WorkflowToolTarget::Start {
                    start: start.clone(),
                },
                crate::WorkflowToolCompletion::Promises {
                    reply_schema_ref: None,
                    deadline_after_ms: None,
                    max_promises: 1,
                    key_source: crate::WorkflowToolCompletionKeySource::Reply,
                },
            )],
        );
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: config(),
                    session_universe_id: universe_id,
                    workflow_tools: declaration,
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, open);
        install_test_tool(&mut drive, "launch_job");
        request_run(&mut drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(&mut drive);
        let request = drive_until_tool_batch_request(&mut drive, generation, "launch_job");
        let binding = drive
            .state()
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
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
        let promise_id = crate::PromiseId::from_number(1);
        let execution_id =
            crate::workflow_tool_execution_id(&invocation_id, &start.recipe_fingerprint);
        let invocation = WorkflowToolInvocation {
            invocation_id: invocation_id.clone(),
            tool_id: definition.tool_id,
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
            execution_context_ref: None,
            completion_promises: Some(std::collections::BTreeMap::from([(
                crate::REPLY_COMPLETION_KEY.to_owned(),
                promise_id.clone(),
            )])),
        };
        let mut result = completed_tool_result(&request);
        result.results[0].effects = vec![crate::workflow_tool_emit_effect(&invocation)];

        let resumed = drive
            .resume_tool_batch(result, 90)
            .expect("resume start-on-call tool");
        let CoreAgentAction::AppendEvents { events, .. } = &resumed else {
            panic!("expected append");
        };
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event.kind, "lightspeed.core.tool.call_completed");
        assert_eq!(events[1].event.kind, "lightspeed.core.promise.created");
        assert_eq!(
            events[2].event.kind,
            "lightspeed.core.workflow_tool.start_requested"
        );
        commit_action(&mut drive, resumed);

        let recorded = drive
            .state()
            .workflow_tools
            .start_requests
            .get(&invocation_id)
            .expect("durable start intent");
        assert_eq!(recorded, &invocation);
        assert!(drive.state().workflow_tools.emissions.is_empty());
        let promise = drive
            .state()
            .promises
            .promises
            .get(&promise_id)
            .expect("keyed completion promise");
        assert_eq!(
            promise.source,
            crate::PromiseSource::Workflow {
                producer_workflow_id: execution_id.clone(),
                producer_workflow_kind: crate::WORKFLOW_TOOL_EXECUTION_KIND.to_owned(),
                invocation_id: invocation_id.as_str().to_owned(),
                completion_key: crate::REPLY_COMPLETION_KEY.to_owned(),
            }
        );

        // Terminal start failure fails the still-pending keyed promise
        // atomically with the StartFailed fact; retry is a no-op.
        let error_ref = BlobRef::from_bytes(b"start worker unreachable");
        let failed = drive
            .admit_command(
                CoreAgentCommand::FailWorkflowToolStart {
                    invocation_id: invocation_id.clone(),
                    error_ref: error_ref.clone(),
                },
                95,
            )
            .expect("terminal start failure");
        let entries = commit_action(&mut drive, failed);
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            entries[0].event,
            CoreAgentEvent::WorkflowTool(crate::WorkflowToolEvent::StartFailed { .. })
        ));
        assert!(matches!(
            entries[1].event,
            CoreAgentEvent::Promise(crate::PromiseEvent::Failed { .. })
        ));
        assert_eq!(
            drive
                .state()
                .promises
                .promises
                .get(&promise_id)
                .expect("failed promise")
                .status,
            crate::PromiseStatus::Failed
        );
        let retry = drive
            .admit_command(
                CoreAgentCommand::FailWorkflowToolStart {
                    invocation_id,
                    error_ref,
                },
                96,
            )
            .expect("idempotent retry");
        assert!(
            !matches!(retry, CoreAgentAction::AppendEvents { .. }),
            "start-failure retry must be a no-op: {retry:?}"
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
                ToolBatchOutcome::completed(promise_tool_result(&request, "promise_1")),
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
            .get(&crate::PromiseId::new("promise_1"))
            .expect("promise in state");
        assert_eq!(promise.status, crate::PromiseStatus::Pending);
        assert_eq!(promise.scope, crate::PromiseScope::Run { run_id });
    }

    fn promise_tool_result_with_ids(
        request: &ToolInvocationBatchRequest,
        numbers: &[u64],
    ) -> ToolInvocationBatchResult {
        let mut result = completed_tool_result(request);
        result.results[0].effects = numbers
            .iter()
            .map(|number| {
                crate::promise_create_effect(
                    &crate::PromiseId::from_number(*number),
                    &crate::PromiseSource::Timer {
                        fire_at_ms: u64::MAX,
                    },
                    None,
                )
            })
            .collect();
        result
    }

    #[test]
    fn tool_batch_promise_base_sits_above_the_cursor_and_the_cursor_follows_the_max() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        assert_eq!(request.promise_id_base, 1);

        // Executors of parallel calls draw from the allocator in any order;
        // every id at or above the base is accepted and the cursor follows
        // the highest one.
        let resumed = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::completed(promise_tool_result_with_ids(&request, &[3, 2])),
                90,
            )
            .expect("resume tool batch");
        commit_action(&mut drive, resumed);
        let promises = &drive.state().promises.promises;
        assert!(promises.contains_key(&crate::PromiseId::from_number(2)));
        assert!(promises.contains_key(&crate::PromiseId::from_number(3)));
        assert_eq!(drive.state().id_cursors.last_promise_id, 3);
    }

    #[test]
    fn tool_result_promise_below_the_batch_base_is_an_invariant_violation() {
        let session_id = SessionId::new("session-a");
        let mut state = CoreAgentState::new();
        state.id_cursors.last_promise_id = 5;
        let mut drive = CoreAgentDrive::from_replayed(session_id, state, None);
        let request = drive_to_single_tool_invocation(&mut drive);
        assert_eq!(request.promise_id_base, 6);

        assert!(
            drive
                .resume_tool_batch_outcome(
                    ToolBatchOutcome::completed(promise_tool_result_with_ids(&request, &[2])),
                    90,
                )
                .is_err(),
            "an id below the batch base could collide with an earlier promise"
        );
        let resumed = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::completed(promise_tool_result_with_ids(&request, &[6])),
                90,
            )
            .expect("resume tool batch");
        commit_action(&mut drive, resumed);
        assert_eq!(drive.state().id_cursors.last_promise_id, 6);
    }

    #[test]
    fn tool_result_reusing_a_promise_id_is_an_invariant_violation() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        assert!(
            drive
                .resume_tool_batch_outcome(
                    ToolBatchOutcome::completed(promise_tool_result_with_ids(&request, &[1, 1])),
                    90,
                )
                .is_err()
        );
    }

    #[test]
    fn tool_result_detach_effect_promotes_promise_to_session_scope() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let run_id = request.run_id;
        let promise_id = crate::PromiseId::new("promise_1");
        drive.state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Timer {
                    fire_at_ms: u64::MAX,
                },
                scope: crate::PromiseScope::Run { run_id },
                ownership: crate::PromiseOwnership::Model,
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
        let promise_id = crate::PromiseId::new("promise_1");
        state.promises.promises.insert(
            promise_id.clone(),
            crate::Promise {
                promise_id: promise_id.clone(),
                source: crate::PromiseSource::Timer {
                    fire_at_ms: u64::MAX,
                },
                scope: crate::PromiseScope::Session,
                ownership: crate::PromiseOwnership::Model,
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
                    output: None,
                }),
            )],
        );

        assert_eq!(proposals.len(), 1);
    }

    #[test]
    fn promise_control_argument_facts_join_only_requested_state() {
        let mut state = CoreAgentState::new();
        state.promises.promises.insert(
            crate::PromiseId::new("promise_1"),
            crate::Promise {
                promise_id: crate::PromiseId::new("promise_1"),
                source: crate::PromiseSource::Timer { fire_at_ms: 10 },
                scope: crate::PromiseScope::Run {
                    run_id: RunId::new(4),
                },
                ownership: crate::PromiseOwnership::Model,
                status: crate::PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );
        let request = ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(4),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: None,
            calls: vec![
                ToolInvocationRequest {
                    builtin: None,
                    call_id: crate::ToolCallId::new("cancel"),
                    tool_id: Some(ToolName::new("concurrency.cancel")),
                    tool_name: ToolName::new("cancel"),
                    arguments_ref: BlobRef::from_bytes(b"cancel"),
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
                },
                ToolInvocationRequest {
                    builtin: None,
                    call_id: crate::ToolCallId::new("detach-invalid"),
                    tool_id: Some(ToolName::new("concurrency.detach")),
                    tool_name: ToolName::new("detach"),
                    arguments_ref: BlobRef::from_bytes(b"detach"),
                    workflow_tool: None,
                    promise_control: None,
                    remote_mcp: None,
                },
            ],
        };
        let joined = attach_promise_control_runtime(
            &state,
            request,
            crate::PromiseControlArgumentFacts {
                version: crate::PromiseControlArgumentFacts::VERSION,
                calls: vec![
                    crate::PromiseControlArgumentCallFacts::Parsed {
                        call_id: crate::ToolCallId::new("cancel"),
                        promise_ids: vec![
                            crate::PromiseId::new("promise_1"),
                            crate::PromiseId::new("promise_99"),
                        ],
                    },
                    crate::PromiseControlArgumentCallFacts::Invalid {
                        call_id: crate::ToolCallId::new("detach-invalid"),
                    },
                ],
            },
        )
        .expect("join promise controls");

        let controls = &joined.calls[0]
            .promise_control
            .as_ref()
            .expect("cancel runtime")
            .controls;
        assert_eq!(controls.len(), 2);
        assert!(matches!(
            controls[0].state,
            crate::PromiseControlStateRuntime::Known {
                ownership: crate::PromiseOwnership::Model,
                scope: crate::PromiseScope::Run { run_id },
                promise_status: crate::PromiseStatus::Pending,
            } if run_id == RunId::new(4)
        ));
        assert_eq!(
            controls[1].state,
            crate::PromiseControlStateRuntime::Unknown
        );
        assert!(joined.calls[1].promise_control.is_none());
    }

    #[test]
    fn resolve_promise_is_first_writer_wins_and_rejects_unknown_ids() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = drive_to_single_tool_invocation(&mut drive);
        let resumed = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::completed(promise_tool_result(&request, "promise_1")),
                90,
            )
            .expect("resume tool batch");
        commit_action(&mut drive, resumed);

        let payload_ref = BlobRef::from_bytes(b"child output");
        let resolve = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: crate::PromiseId::new("promise_1"),
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
            .get(&crate::PromiseId::new("promise_1"))
            .expect("promise in state");
        assert_eq!(promise.status, crate::PromiseStatus::Resolved);
        assert_eq!(promise.payload_ref.as_ref(), Some(&payload_ref));

        // First writer wins: a late conflicting delivery is a no-op.
        let late = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: crate::PromiseId::new("promise_1"),
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
                .get(&crate::PromiseId::new("promise_1"))
                .expect("promise")
                .status,
            crate::PromiseStatus::Resolved
        );

        let unknown = drive
            .admit_command(
                CoreAgentCommand::ResolvePromise {
                    promise_id: crate::PromiseId::new("promise_99"),
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

    fn install_test_tools(drive: &mut CoreAgentDrive, tool_names: &[&str]) {
        let tools = tool_names
            .iter()
            .map(|tool_name| {
                let spec = test_tool_spec(tool_name);
                (spec.name.clone(), spec)
            })
            .collect();
        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(drive.state().tooling.revision),
                    tools,
                },
                15,
            )
            .expect("replace tools");
        commit_action(drive, action);
    }

    fn two_call_tool_batch(
        drive: &mut CoreAgentDrive,
        session_config: SessionConfig,
    ) -> ToolInvocationBatchRequest {
        open_session_with_config(drive, session_config);
        install_test_tools(drive, &["tool_a", "tool_b"]);
        request_run(drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(drive);
        drive_until_tool_batch_request_with_calls(
            drive,
            generation,
            vec![
                ObservedToolCall {
                    call_id: crate::ToolCallId::new("call_a"),
                    tool_id: Some(ToolName::new("tool_a")),
                    tool_name: ToolName::new("tool_a"),
                    provider_kind: None,
                    arguments_ref: BlobRef::from_bytes(br#"{"a":true}"#),
                    native_call_ref: None,
                },
                ObservedToolCall {
                    call_id: crate::ToolCallId::new("call_b"),
                    tool_id: Some(ToolName::new("tool_b")),
                    tool_name: ToolName::new("tool_b"),
                    provider_kind: None,
                    arguments_ref: BlobRef::from_bytes(br#"{"b":true}"#),
                    native_call_ref: None,
                },
            ],
        )
    }

    fn per_call_result(
        call_id: &crate::ToolCallId,
        status: ToolCallStatus,
        effects: Vec<ToolEffect>,
    ) -> ToolInvocationResult {
        let content_ref = BlobRef::from_bytes(call_id.as_str().as_bytes());
        ToolInvocationResult {
            duration_ms: None,
            output_bytes: None,
            truncated: false,
            call_id: call_id.clone(),
            status,
            output_ref: (status == ToolCallStatus::Succeeded).then(|| content_ref.clone()),
            model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
                call_id,
                status,
                content_ref.clone(),
            )],
            error_ref: (status != ToolCallStatus::Succeeded).then_some(content_ref),
            effects,
        }
    }

    #[test]
    fn per_call_resumes_complete_a_batch_progressively() {
        let session_id = SessionId::new("session-per-call");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = two_call_tool_batch(&mut drive, config());
        assert_eq!(request.calls.len(), 2);

        // First call completes on its own; its sibling stays pending and the
        // batch stays active.
        let first = drive
            .resume_tool_call(
                request.batch_id,
                per_call_result(
                    &request.calls[0].call_id,
                    ToolCallStatus::Succeeded,
                    Vec::new(),
                ),
                121,
            )
            .expect("resume first call");
        commit_action(&mut drive, first);
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let batch = active_run
            .tool_batches
            .get(&request.batch_id)
            .expect("active batch");
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);
        assert_eq!(batch.calls[1].status, ToolCallStatus::Pending);
        assert_eq!(active_run.active_tool_batch_id, Some(request.batch_id));

        // A duplicate completion for the first call is rejected without
        // touching the sibling.
        drive
            .resume_tool_call(
                request.batch_id,
                per_call_result(
                    &request.calls[0].call_id,
                    ToolCallStatus::Succeeded,
                    Vec::new(),
                ),
                122,
            )
            .expect_err("completed call cannot complete again");

        // The second call fails terminally; the completed sibling result is
        // untouched and the batch completes once every call is terminal.
        let second = drive
            .resume_tool_call(
                request.batch_id,
                per_call_result(
                    &request.calls[1].call_id,
                    ToolCallStatus::Failed,
                    Vec::new(),
                ),
                123,
            )
            .expect("resume second call");
        commit_action(&mut drive, second);
        for observed_at_ms in 124..140 {
            let action = drive.next_action(observed_at_ms, 64).expect("next action");
            if matches!(
                action,
                CoreAgentAction::GenerateLlm { .. } | CoreAgentAction::Idle
            ) {
                break;
            }
            commit_action(&mut drive, action);
        }
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let completed = active_run
            .completed_tool_batches
            .get(&request.batch_id)
            .expect("completed batch");
        assert_eq!(completed.results.len(), 2);
        assert_eq!(completed.results[0].status, ToolCallStatus::Succeeded);
        assert_eq!(completed.results[1].status, ToolCallStatus::Failed);
        assert_ne!(active_run.active_tool_batch_id, Some(request.batch_id));
    }

    /// Runtime-measured per-call durations recorded by the execution
    /// activity survive on the stored tool results, where analytics and
    /// projections read them back per call.
    #[test]
    fn per_call_results_retain_runtime_durations() {
        let session_id = SessionId::new("session-durations");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = two_call_tool_batch(&mut drive, config());

        let mut first = per_call_result(
            &request.calls[0].call_id,
            ToolCallStatus::Succeeded,
            Vec::new(),
        );
        first.duration_ms = Some(400);
        let first = drive
            .resume_tool_call(request.batch_id, first, 121)
            .expect("resume first call");
        commit_action(&mut drive, first);

        let mut second = per_call_result(
            &request.calls[1].call_id,
            ToolCallStatus::Succeeded,
            Vec::new(),
        );
        second.duration_ms = Some(600);
        let second = drive
            .resume_tool_call(request.batch_id, second, 122)
            .expect("resume second call");
        commit_action(&mut drive, second);

        loop {
            let action = drive.next_action(140, 64).expect("next action");
            if matches!(
                action,
                CoreAgentAction::GenerateLlm { .. } | CoreAgentAction::Idle
            ) {
                break;
            }
            commit_action(&mut drive, action);
        }
        let active_run = drive.state().runs.active.as_ref().expect("active run");
        let completed_batch = active_run
            .completed_tool_batches
            .get(&request.batch_id)
            .expect("completed batch");
        assert_eq!(completed_batch.results[0].duration_ms, Some(400));
        assert_eq!(completed_batch.results[1].duration_ms, Some(600));
    }

    #[test]
    fn per_call_environment_selection_stays_exclusive_across_resumes() {
        let session_id = SessionId::new("session-per-call-environment");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let mut session_config = config();
        session_config.features.environments = Some(crate::EnvironmentsFeature {
            selection_tools: true,
            ..crate::EnvironmentsFeature::default()
        });
        let request = two_call_tool_batch(&mut drive, session_config);
        let activate_effect =
            crate::environment_activate_effect(&crate::EnvironmentId::new("environment-a"));

        let first = drive
            .resume_tool_call(
                request.batch_id,
                per_call_result(
                    &request.calls[0].call_id,
                    ToolCallStatus::Succeeded,
                    vec![activate_effect.clone()],
                ),
                121,
            )
            .expect("first selection effect is admitted");
        commit_action(&mut drive, first);

        // The exclusivity invariant spans the whole batch: a second selection
        // effect arriving through a later per-call resume must be rejected.
        let error = drive
            .resume_tool_call(
                request.batch_id,
                per_call_result(
                    &request.calls[1].call_id,
                    ToolCallStatus::Succeeded,
                    vec![activate_effect],
                ),
                122,
            )
            .expect_err("second selection effect in the same batch is rejected");
        assert!(matches!(
            error,
            CoreAgentDriveError::Domain(DomainError::InvariantViolation(_))
        ));
    }

    fn native_inject_mcp_tool(name: &str, approval: crate::RemoteMcpApprovalPolicy) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(crate::RemoteMcpToolSpec {
                server_id: "echo".to_owned(),
                record_revision: 1,
                server_label: "echo".to_owned(),
                server_url: "https://echo.example.com/mcp".to_owned(),
                description_ref: None,
                allowed_tools: Some(vec!["hello".to_owned()]),
                execution: crate::RemoteMcpExecution::Native,
                exposure: crate::RemoteMcpExposure::Inject,
                approval,
                defer_loading: None,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            parallelism: ToolParallelism::ParallelSafe,
        }
    }

    fn observed_call(call_id: &str, tool_name: &str) -> ObservedToolCall {
        ObservedToolCall {
            call_id: crate::ToolCallId::new(call_id),
            tool_id: Some(test_tool_id(tool_name)),
            tool_name: ToolName::new(tool_name),
            provider_kind: None,
            arguments_ref: BlobRef::from_bytes(b"{}"),
            native_call_ref: None,
        }
    }

    /// One model turn mixing an ordinary tool with two injected native MCP
    /// calls whose server requires approval for every call.
    fn mixed_native_mcp_batch(drive: &mut CoreAgentDrive) -> ToolInvocationBatchRequest {
        open_session(drive);
        let tool_a = test_tool_spec("tool_a");
        let mcp = native_inject_mcp_tool("mcp_echo", crate::RemoteMcpApprovalPolicy::Always);
        let tools = BTreeMap::from([(tool_a.name.clone(), tool_a), (mcp.name.clone(), mcp)]);
        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(drive.state().tooling.revision),
                    tools,
                },
                15,
            )
            .expect("replace tools");
        commit_action(drive, action);
        request_run(drive, BlobRef::from_bytes(b"input"));
        let generation = drive_until_generate(drive);
        drive_until_tool_batch_request_with_calls(
            drive,
            generation,
            vec![
                observed_call("call_a", "tool_a"),
                observed_call("call_m1", "mcp_echo__hello"),
                observed_call("call_m2", "mcp_echo__hello"),
            ],
        )
    }

    fn native_mcp_approval(call_id: &crate::ToolCallId) -> crate::NativeMcpApprovalRequest {
        crate::NativeMcpApprovalRequest {
            call_id: call_id.clone(),
            subject: crate::ApprovalSubject::McpToolCall {
                server_id: "echo".to_owned(),
                server_label: "echo".to_owned(),
                tool_name: "hello".to_owned(),
                arguments_ref: BlobRef::from_bytes(b"{}"),
            },
        }
    }

    fn decide_native_mcp_approval(
        drive: &mut CoreAgentDrive,
        run_id: RunId,
        number: u64,
        decision: crate::ApprovalDecision,
        observed_at_ms: u64,
    ) {
        let action = drive
            .admit_command(
                CoreAgentCommand::DecideApproval(crate::ApprovalDecisionCommand {
                    approval_id: crate::ApprovalId::new(format!("approval_{number}")),
                    run_id,
                    decision,
                    note: None,
                    decided_by: None,
                    response: None,
                }),
                observed_at_ms,
            )
            .expect("decide approval");
        commit_action(drive, action);
    }

    fn next_tool_batch_dispatch(
        drive: &mut CoreAgentDrive,
        from_ms: u64,
    ) -> ToolInvocationBatchRequest {
        for observed_at_ms in from_ms..from_ms + 20 {
            match drive.next_action(observed_at_ms, 64).expect("next action") {
                CoreAgentAction::InvokeTools { request } => return request,
                action @ CoreAgentAction::AppendEvents { .. } => {
                    commit_action(drive, action);
                }
                other => panic!("unexpected action before tool batch re-dispatch: {other:?}"),
            }
        }
        panic!("drive did not re-dispatch the tool batch");
    }

    fn injected_decision(call: &ToolInvocationRequest) -> Option<bool> {
        match &call.remote_mcp {
            Some(crate::RemoteMcpCallRuntime::Injected {
                target,
                remote_tool_name,
                approval_decision,
            }) => {
                assert_eq!(target.server_id, "echo");
                assert_eq!(target.approval, crate::RemoteMcpApprovalPolicy::Always);
                assert_eq!(remote_tool_name, "hello");
                *approval_decision
            }
            other => panic!("injected call must carry native MCP routing, got {other:?}"),
        }
    }

    #[test]
    fn batch_requests_carry_native_mcp_routing_for_every_call_that_needs_it() {
        let session_id = SessionId::new("session-mixed-native-mcp-routing");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = mixed_native_mcp_batch(&mut drive);

        assert_eq!(request.calls.len(), 3);
        assert!(request.calls[0].remote_mcp.is_none());
        assert_eq!(injected_decision(&request.calls[1]), None);
        assert_eq!(injected_decision(&request.calls[2]), None);
    }

    #[test]
    fn awaiting_approval_outcome_completes_ungated_calls_and_parks_once_for_the_gated_set() {
        let session_id = SessionId::new("session-mixed-native-mcp-approvals");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = mixed_native_mcp_batch(&mut drive);
        let run_id = request.run_id;

        // The batch activity ran the ungated sibling and reports both gated
        // calls; everything appends as one action.
        let parked = drive
            .resume_tool_batch_outcome(
                ToolBatchOutcome::AwaitingApproval {
                    batch_id: request.batch_id,
                    completed_results: vec![per_call_result(
                        &request.calls[0].call_id,
                        ToolCallStatus::Succeeded,
                        Vec::new(),
                    )],
                    approvals: vec![
                        native_mcp_approval(&request.calls[1].call_id),
                        native_mcp_approval(&request.calls[2].call_id),
                    ],
                },
                90,
            )
            .expect("park on approvals");
        commit_action(&mut drive, parked);

        let run = drive.state().runs.active.as_ref().expect("active run");
        assert_eq!(run.status, RunStatus::Parked);
        assert_eq!(run.pending_approvals().count(), 2);
        let batch = run
            .tool_batches
            .get(&request.batch_id)
            .expect("active batch");
        assert_eq!(batch.calls[0].status, ToolCallStatus::Succeeded);
        assert_eq!(batch.calls[1].status, ToolCallStatus::Pending);
        assert_eq!(batch.calls[2].status, ToolCallStatus::Pending);
        // A parked run asks the runtime for nothing.
        assert!(matches!(
            drive.next_action(91, 64).expect("next action"),
            CoreAgentAction::Idle
        ));

        // Decisions land one at a time; the run unparks only once the set is
        // complete.
        decide_native_mcp_approval(&mut drive, run_id, 1, crate::ApprovalDecision::Approved, 92);
        assert_eq!(
            drive
                .state()
                .runs
                .active
                .as_ref()
                .expect("active run")
                .status,
            RunStatus::Parked
        );
        assert!(matches!(
            drive.next_action(93, 64).expect("next action"),
            CoreAgentAction::Idle
        ));
        decide_native_mcp_approval(&mut drive, run_id, 2, crate::ApprovalDecision::Rejected, 94);

        // The re-dispatch is the same batch, carries only the pending calls,
        // and pins each decision onto its call's routing facts.
        let redispatch = next_tool_batch_dispatch(&mut drive, 95);
        assert_eq!(redispatch.batch_id, request.batch_id);
        assert_eq!(redispatch.promise_id_base, request.promise_id_base);
        assert_eq!(
            redispatch
                .calls
                .iter()
                .map(|call| call.call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["call_m1", "call_m2"]
        );
        assert_eq!(injected_decision(&redispatch.calls[0]), Some(true));
        assert_eq!(injected_decision(&redispatch.calls[1]), Some(false));
    }

    #[test]
    fn native_mcp_approval_park_requires_pending_calls_of_the_active_batch() {
        let session_id = SessionId::new("session-mixed-native-mcp-park-guards");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let request = mixed_native_mcp_batch(&mut drive);

        let empty = drive
            .request_native_mcp_approvals(request.batch_id, Vec::new(), 90)
            .expect_err("an empty gated set must not park the run");
        assert!(matches!(
            empty,
            CoreAgentDriveError::Domain(DomainError::InvariantViolation(_))
        ));
        let unknown = drive
            .request_native_mcp_approvals(
                request.batch_id,
                vec![native_mcp_approval(&crate::ToolCallId::new("call_unknown"))],
                90,
            )
            .expect_err("an unknown call must not park the run");
        assert!(matches!(
            unknown,
            CoreAgentDriveError::Domain(DomainError::InvariantViolation(_))
        ));
        let duplicate = drive
            .request_native_mcp_approvals(
                request.batch_id,
                vec![
                    native_mcp_approval(&request.calls[1].call_id),
                    native_mcp_approval(&request.calls[1].call_id),
                ],
                90,
            )
            .expect_err("a call may be gated once per park");
        assert!(matches!(
            duplicate,
            CoreAgentDriveError::Domain(DomainError::InvariantViolation(_))
        ));
    }
    #[test]
    fn builtin_calls_replay_with_internal_identity_and_original_turn_inputs() {
        let session_id = SessionId::new("builtin-replay");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let mut session_config = config();
        session_config.model = ModelSelection {
            api_kind: ProviderApiKind::AnthropicMessages,
            provider_id: "anthropic".into(),
            model: "claude-base".into(),
        };
        open_session_with_config(&mut drive, session_config);
        let spec = crate::BuiltinToolSpec {
            settings: serde_json::json!({"one_shot":true}),
        };
        let tool = ToolSpec {
            name: ToolName::new("env.run_process"),
            kind: ToolKind::Builtin(spec.clone()),
            parallelism: ToolParallelism::Exclusive,
            execution: Default::default(),
        };
        let action = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(0),
                    tools: BTreeMap::from([(tool.name.clone(), tool)]),
                },
                11,
            )
            .unwrap();
        commit_action(&mut drive, action);
        let override_model = ModelSelection {
            api_kind: ProviderApiKind::AnthropicMessages,
            provider_id: "anthropic".into(),
            model: "claude-test".into(),
        };
        let action = drive
            .admit_command(
                request_run_command(
                    None,
                    user_input(BlobRef::from_bytes(b"input")),
                    RunConfig {
                        model_override: Some(override_model.clone()),
                        ..Default::default()
                    },
                ),
                20,
            )
            .unwrap();
        commit_action(&mut drive, action);
        let generation = drive_until_generate(&mut drive);
        let checkpoint = serde_json::to_vec(drive.state()).unwrap();
        let head = drive.head().cloned();
        let native_call = BlobRef::from_bytes(
            br#"{"type":"tool_use","name":"Bash","input":{"command":"echo hello"}}"#,
        );
        let call = ObservedToolCall {
            call_id: crate::ToolCallId::new("call-bash"),
            tool_id: Some(ToolName::new("env.run_process")),
            tool_name: ToolName::new("Bash"),
            provider_kind: Some("tool_use".into()),
            arguments_ref: BlobRef::from_bytes(br#"{"command":"echo hello"}"#),
            native_call_ref: Some(native_call.clone()),
        };
        let mut unknown = call.clone();
        unknown.call_id = crate::ToolCallId::new("unknown-alias");
        unknown.tool_name = ToolName::new("exec_command");
        unknown.tool_id = None;
        let action = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: generation.run_id,
                    turn_id: generation.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: vec![ContextEntryInput {
                        kind: ContextEntryKind::ToolCall {
                            call_id: call.call_id.clone(),
                            name: call.tool_name.clone(),
                        },
                        content: crate::ContentRef {
                            content_ref: native_call.clone(),
                            media_type: Some("application/json".into()),
                            provider_kind: Some("tool_use".into()),
                        },

                        origin: None,

                        provenance_ref: None,
                        preview: None,
                        token_estimate: None,
                    }],
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: None,
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: vec![call.clone(), unknown],
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                30,
            )
            .unwrap();
        let mut events = commit_action(&mut drive, action);
        let request = loop {
            match drive.next_action(31, 64).unwrap() {
                CoreAgentAction::InvokeTools { request } => break request,
                action => events.extend(commit_action(&mut drive, action)),
            }
        };
        assert_eq!(request.calls.len(), 1, "unknown aliases do not execute");
        assert_eq!(request.calls[0].tool_id, call.tool_id);
        assert_eq!(request.calls[0].tool_name.as_str(), "Bash");
        assert_eq!(
            request.calls[0].builtin,
            Some(crate::BuiltinToolCallRuntime {
                spec,
                model: override_model
            })
        );
        let mut replayed = CoreAgentDrive::from_replayed(
            session_id,
            serde_json::from_slice(&checkpoint).unwrap(),
            head,
        );
        // Replaying the committed events needs no definitions, blobs, or codecs.
        replayed
            .resume_appended(
                events
                    .iter()
                    .map(|entry| CoreAgentCodec.encode_entry(entry).unwrap())
                    .collect(),
            )
            .unwrap();
        assert_eq!(replayed.state(), drive.state());
        let CoreAgentAction::InvokeTools { request: restored } =
            replayed.next_action(32, 64).unwrap()
        else {
            panic!("replayed invocation");
        };
        assert_eq!(restored, request);
        assert!(replayed.state().context.entries.iter().any(|entry| entry.content.content_ref == native_call && matches!(&entry.kind, ContextEntryKind::ToolCall { name, .. } if name.as_str() == "Bash")));
    }
}
