//! CoreAgent workflow orchestration around deterministic planning and async
//! LLM/tool calls.
//!
//! The workflow interleaves deterministic planning with runtime-provided async
//! I/O traits. It converts replayed session state into runtime requests,
//! awaits LLM/tool implementations, and converts returned results back into
//! domain event proposals.

use crate::{
    ApplyEvent, BlobRef, ContextEvent, ContextItem, ContextItemId, ContextItemKind,
    ContextMessageRole, CoreAgentCodec, CoreAgentEntry, CoreAgentEventKind, CoreAgentEventProposal,
    CoreAgentIoError, CoreAgentJoins, CoreAgentLlm, CoreAgentState, CoreAgentTools, DomainError,
    EventSeq, LlmFinish, LlmGenerationFacts, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, PlanNext, SessionId, SessionPosition, ToolCallResult, ToolCallStatus,
    ToolEvent, ToolInvocationBatchRequest, ToolInvocationBatchResult, ToolInvocationRequest,
    ToolInvocationResult, TurnEvent, TurnOutcome, UncommittedContextItem,
    UncommittedCoreAgentEvent,
    runner::{RunnerError, RunnerQuiescence},
    storage::{AppendSessionEvents, BlobStore, BlobStoreError, SessionStore},
};

pub struct CoreAgentWorkflow<'a> {
    session_id: &'a SessionId,
    sessions: &'a dyn SessionStore,
    blobs: &'a dyn BlobStore,
    apply: &'a dyn ApplyEvent,
    planner: &'a dyn PlanNext,
    llm: &'a dyn CoreAgentLlm,
    tools: Option<&'a dyn CoreAgentTools>,
}

impl<'a> CoreAgentWorkflow<'a> {
    pub fn new(
        session_id: &'a SessionId,
        sessions: &'a dyn SessionStore,
        blobs: &'a dyn BlobStore,
        apply: &'a dyn ApplyEvent,
        planner: &'a dyn PlanNext,
        llm: &'a dyn CoreAgentLlm,
        tools: Option<&'a dyn CoreAgentTools>,
    ) -> Self {
        Self {
            session_id,
            sessions,
            blobs,
            apply,
            planner,
            llm,
            tools,
        }
    }

    pub async fn drive_until_quiescent(
        &self,
        state: &mut CoreAgentState,
        event_buffer: &mut WorkflowEventBuffer,
        observed_at_ms: u64,
        max_steps: usize,
        emitted_entries: &mut Vec<CoreAgentEntry>,
    ) -> Result<RunnerQuiescence, RunnerError> {
        let mut steps = 0usize;
        loop {
            let proposals = self.planner.plan_next(state)?;
            if !proposals.is_empty() {
                let Some(next_steps) = increment_steps(steps, max_steps) else {
                    event_buffer
                        .flush(self.sessions, self.session_id, emitted_entries)
                        .await?;
                    return Ok(RunnerQuiescence::IterationLimitReached);
                };
                steps = next_steps;
                event_buffer.stage_and_apply(self.apply, state, proposals, observed_at_ms)?;
                continue;
            }

            if let Some(request) = next_generation_request(self.session_id, state)? {
                let Some(next_steps) = increment_steps(steps, max_steps) else {
                    event_buffer
                        .flush(self.sessions, self.session_id, emitted_entries)
                        .await?;
                    return Ok(RunnerQuiescence::IterationLimitReached);
                };
                steps = next_steps;
                event_buffer
                    .flush(self.sessions, self.session_id, emitted_entries)
                    .await?;
                let result = match self.llm.generate(request.clone()).await {
                    Ok(result) => result,
                    Err(error) => {
                        failed_generation_result_from_error(self.blobs, request, error).await?
                    }
                };
                let proposals = generation_result_proposals(state, result)?;
                event_buffer.stage_and_apply(self.apply, state, proposals, observed_at_ms)?;
                continue;
            }

            if let Some(request) = next_tool_batch_request(self.session_id, state)? {
                let Some(next_steps) = increment_steps(steps, max_steps) else {
                    event_buffer
                        .flush(self.sessions, self.session_id, emitted_entries)
                        .await?;
                    return Ok(RunnerQuiescence::IterationLimitReached);
                };
                steps = next_steps;
                let Some(tools) = self.tools else {
                    let result = failed_tool_batch_result(
                        self.blobs,
                        &request,
                        "agent-core tool runtime unavailable",
                    )
                    .await?;
                    let proposals = tool_batch_result_proposals(result)?;
                    event_buffer.stage_and_apply(self.apply, state, proposals, observed_at_ms)?;
                    continue;
                };
                event_buffer
                    .flush(self.sessions, self.session_id, emitted_entries)
                    .await?;
                let result = match tools.invoke_batch(request.clone()).await {
                    Ok(result) => result,
                    Err(error) => {
                        failed_tool_batch_result(self.blobs, &request, error.to_string()).await?
                    }
                };
                let proposals = tool_batch_result_proposals(result)?;
                event_buffer.stage_and_apply(self.apply, state, proposals, observed_at_ms)?;
                continue;
            }

            event_buffer
                .flush(self.sessions, self.session_id, emitted_entries)
                .await?;
            return Ok(classify_quiescence(state));
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowEventBuffer {
    durable_head: Option<SessionPosition>,
    codec: CoreAgentCodec,
    events: Vec<UncommittedCoreAgentEvent>,
    staged_entries: Vec<CoreAgentEntry>,
}

impl WorkflowEventBuffer {
    pub fn new(durable_head: Option<SessionPosition>) -> Self {
        Self {
            durable_head,
            codec: CoreAgentCodec,
            events: Vec::new(),
            staged_entries: Vec::new(),
        }
    }

    pub fn stage_and_apply(
        &mut self,
        apply: &dyn ApplyEvent,
        state: &mut CoreAgentState,
        proposals: Vec<CoreAgentEventProposal>,
        observed_at_ms: u64,
    ) -> Result<(), RunnerError> {
        for proposal in proposals {
            let event = proposal.into_uncommitted(observed_at_ms);
            let entry = staged_entry(state.reduced_to.as_ref(), &event)?;
            apply.apply(state, &entry)?;
            self.events.push(event);
            self.staged_entries.push(entry);
        }
        Ok(())
    }

    pub async fn flush(
        &mut self,
        sessions: &dyn SessionStore,
        session_id: &SessionId,
        emitted_entries: &mut Vec<CoreAgentEntry>,
    ) -> Result<(), RunnerError> {
        if self.events.is_empty() {
            return Ok(());
        }

        let events = self
            .events
            .iter()
            .map(|event| self.codec.encode_uncommitted(event))
            .collect::<Result<Vec<_>, _>>()?;

        let appended = sessions
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: self.durable_head.clone(),
                events,
            })
            .await?;

        let appended_entries = appended
            .entries
            .iter()
            .map(|entry| self.codec.decode_entry(entry))
            .collect::<Result<Vec<_>, _>>()?;

        if appended_entries != self.staged_entries {
            return Err(DomainError::EventOrdering(
                "session store returned entries that differ from staged entries".into(),
            )
            .into());
        }

        self.durable_head = appended.head.clone();
        self.events.clear();
        self.staged_entries.clear();
        emitted_entries.extend(appended_entries);
        Ok(())
    }
}

fn staged_entry(
    current_head: Option<&SessionPosition>,
    event: &UncommittedCoreAgentEvent,
) -> Result<CoreAgentEntry, DomainError> {
    let next_seq =
        match current_head {
            Some(position) => position.seq.as_u64().checked_add(1).ok_or_else(|| {
                DomainError::EventOrdering("session event sequence exhausted".into())
            })?,
            None => 1,
        };
    Ok(CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(next_seq),
        },
        observed_at_ms: event.observed_at_ms,
        joins: event.joins.clone(),
        event: event.event.clone(),
    })
}

fn increment_steps(steps: usize, max_steps: usize) -> Option<usize> {
    if steps >= max_steps {
        return None;
    }
    Some(steps + 1)
}

pub(crate) fn classify_quiescence(state: &CoreAgentState) -> RunnerQuiescence {
    if state.lifecycle.status == crate::CoreAgentStatus::Closed {
        RunnerQuiescence::Closed
    } else {
        RunnerQuiescence::Idle
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
    let request = turn.request.clone().ok_or_else(|| {
        DomainError::InvariantViolation("generation-pending turn is missing request".into())
    })?;
    Ok(Some(LlmGenerationRequest {
        session_id: session_id.clone(),
        run_id: active_run.run_id,
        turn_id,
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
    let context_items = context_items_from_uncommitted(state, &result.context_items)?;
    let outcome = turn_outcome_for_generation_result(&result);
    let joins = CoreAgentJoins {
        run_id: Some(result.run_id),
        turn_id: Some(result.turn_id),
        ..CoreAgentJoins::default()
    };

    let mut proposals = Vec::new();
    if !context_items.is_empty() {
        proposals.push(CoreAgentEventProposal::new(
            joins.clone(),
            CoreAgentEventKind::Context(ContextEvent::ItemsRecorded {
                items: context_items,
            }),
        ));
    }
    if let Some(record) = result.facts.compaction.clone() {
        proposals.push(CoreAgentEventProposal::new(
            joins.clone(),
            CoreAgentEventKind::Context(ContextEvent::CompactionRecorded {
                run_id: result.run_id,
                turn_id: Some(result.turn_id),
                record,
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

pub async fn failed_generation_result_from_error(
    blobs: &dyn BlobStore,
    request: LlmGenerationRequest,
    error: CoreAgentIoError,
) -> Result<LlmGenerationResult, BlobStoreError> {
    let failure_ref = write_error_blob(
        blobs,
        format!(
            "core agent LLM generation failed\nrun_id={}\nturn_id={}\nerror={error}\n",
            request.run_id, request.turn_id
        ),
    )
    .await?;
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status: LlmGenerationStatus::Failed,
        failure_ref: Some(failure_ref),
        context_items: Vec::new(),
        facts: LlmGenerationFacts {
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
            context_token_estimate: None,
            compaction: None,
        },
    })
}

fn context_items_from_uncommitted(
    state: &CoreAgentState,
    uncommitted: &[UncommittedContextItem],
) -> Result<Vec<ContextItem>, DomainError> {
    let mut next_item_id = state.id_cursors.last_context_item_id;
    uncommitted
        .iter()
        .map(|item| {
            next_item_id = next_item_id.checked_add(1).ok_or_else(|| {
                DomainError::InvariantViolation("context item id cursor exhausted".to_owned())
            })?;
            Ok(ContextItem {
                item_id: ContextItemId::new(next_item_id),
                kind: item.kind.clone(),
                source: item.source.clone(),
                native_item_ref: item.native_item_ref.clone(),
                media_type: item.media_type.clone(),
                preview: item.preview.clone(),
                provider_kind: item.provider_kind.clone(),
                provider_item_id: item.provider_item_id.clone(),
                token_estimate: item.token_estimate.clone(),
            })
        })
        .collect()
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
                    output_ref: final_output_ref(&result.context_items),
                }
            }
        },
    }
}

fn final_output_ref(context_items: &[UncommittedContextItem]) -> Option<BlobRef> {
    context_items
        .iter()
        .rev()
        .find_map(|item| match item.kind {
            ContextItemKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.native_item_ref.clone()),
            _ => None,
        })
        .or_else(|| {
            context_items
                .last()
                .map(|item| item.native_item_ref.clone())
        })
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
        calls,
    }))
}

pub fn tool_batch_result_proposals(
    result: ToolInvocationBatchResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    validate_tool_batch_result(&result)?;
    Ok(result
        .results
        .into_iter()
        .map(|result_item| {
            let joins = CoreAgentJoins {
                run_id: Some(result.run_id),
                turn_id: Some(result.turn_id),
                tool_batch_id: Some(result.batch_id),
                tool_call_id: Some(result_item.call_id.clone()),
                ..CoreAgentJoins::default()
            };
            CoreAgentEventProposal::new(
                joins,
                CoreAgentEventKind::Tool(ToolEvent::CallCompleted {
                    run_id: result.run_id,
                    turn_id: result.turn_id,
                    batch_id: result.batch_id,
                    result: ToolCallResult {
                        call_id: result_item.call_id,
                        status: result_item.status,
                        output_ref: result_item.output_ref,
                        model_visible_output_ref: result_item.model_visible_output_ref,
                        error_ref: result_item.error_ref,
                    },
                }),
            )
        })
        .collect())
}

fn validate_tool_batch_result(result: &ToolInvocationBatchResult) -> Result<(), DomainError> {
    for result in &result.results {
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

pub async fn failed_tool_batch_result(
    blobs: &dyn BlobStore,
    request: &ToolInvocationBatchRequest,
    error: impl AsRef<str>,
) -> Result<ToolInvocationBatchResult, BlobStoreError> {
    let mut results = Vec::with_capacity(request.calls.len());
    for call in &request.calls {
        let error_ref = write_error_blob(
            blobs,
            format!(
                "{}\nrun_id={}\nturn_id={}\nbatch_id={}\ncall_id={}\ntool_name={}\n",
                error.as_ref(),
                request.run_id,
                request.turn_id,
                request.batch_id,
                call.call_id,
                call.tool_name
            ),
        )
        .await?;
        results.push(ToolInvocationResult {
            call_id: call.call_id.clone(),
            status: ToolCallStatus::Failed,
            output_ref: None,
            model_visible_output_ref: Some(error_ref.clone()),
            error_ref: Some(error_ref),
        });
    }
    Ok(ToolInvocationBatchResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        batch_id: request.batch_id,
        results,
    })
}

async fn write_error_blob(
    blobs: &dyn BlobStore,
    message: impl Into<String>,
) -> Result<BlobRef, BlobStoreError> {
    blobs.put_bytes(message.into().into_bytes()).await
}
