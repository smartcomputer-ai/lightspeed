//! Substrate-neutral CoreAgent drive machine.
//!
//! The drive machine owns deterministic CoreAgent state and decides the next
//! action required to make progress. It does not perform async I/O, call
//! providers, invoke tools, or write storage. Local runtimes and workflow
//! substrates fulfill emitted actions and resume the drive with committed
//! entries or execution results.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AdmitCommand, ApplyEvent, BlobRef, CodecError, CommandError, ContextCompactionRequest,
    ContextCompactionResult, ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextEvent,
    ContextMessageRole, CoreAdmitCommand, CoreAgentCodec, CoreAgentEntry, CoreAgentEventKind,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus, CoreApplyEvent,
    CorePlanner, DomainError, LlmFinish, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, PlanNext, PlanningError, SessionId, SessionPosition, ToolCallResult,
    ToolCallStatus, ToolEvent, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationRequest, TurnEvent, TurnOutcome,
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
        let proposals = tool_batch_result_proposals(result)?;
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
                        effects: result_item.effects,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlobRef, CommandRejectionKind, CompactionPolicy, ContextCompactionRequestKind,
        ContextCompactionStatus, ContextCompactionTrigger, ContextConfig, ContextConfigPatch,
        ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKey, ContextEntryKind,
        ContextRemovalReason, ContextRewriteReason, CoreAgentCommand, LlmGenerationFacts,
        LlmRequestKind, ModelProviderOptions, ModelSelection,
        OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND, OptionalConfigPatch, ProviderApiKind,
        ProviderRequestDefaults, RunConfig, RunFailureKind, RunStatus,
        SKILL_ACTIVATION_PROVIDER_KIND_RUN, SKILL_CATALOG_CONTEXT_KEY, SessionConfig,
        SessionConfigPatch, SkillId, TokenEstimate, TokenEstimateQuality, TurnConfig,
        TurnConfigPatch, TurnStatus, skill_activation_context_key,
    };

    fn config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
                options: ModelProviderOptions::None,
            },
            run: RunConfig {
                max_turns: None,
                max_tool_rounds: None,
                model_override: None,
                max_output_tokens: None,
                provider_request_defaults: None,
            },
            turn: TurnConfig {
                max_output_tokens: None,
                provider_request_defaults: ProviderRequestDefaults::None,
            },
            context: ContextConfig { compaction: None },
        }
    }

    fn run_config() -> RunConfig {
        RunConfig {
            max_turns: None,
            max_tool_rounds: None,
            model_override: None,
            max_output_tokens: None,
            provider_request_defaults: None,
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
        for observed_at_ms in 21..80 {
            let action = drive.next_action(observed_at_ms, 64).expect("next action");
            if let CoreAgentAction::GenerateLlm { request } = action {
                return request;
            }
            commit_action(drive, action);
        }
        panic!("drive did not emit an LLM action");
    }

    fn openai_items(request: &LlmGenerationRequest) -> &[ContextEntry] {
        let LlmRequestKind::OpenAiResponses(openai) = &request.request.kind else {
            panic!("expected OpenAI Responses request");
        };
        &openai.input_context.entries
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
    fn upsert_context_rejects_user_message_entry() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let error = drive
            .admit_command(
                CoreAgentCommand::UpsertContext {
                    key: ContextEntryKey::new("client.message"),
                    entry: message_input(
                        ContextMessageRole::User,
                        BlobRef::from_bytes(b"persistent user message"),
                    ),
                },
                20,
            )
            .expect_err("user-message context edit must be rejected");

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
        let ContextCompactionRequestKind::OpenAiResponses(openai_request) = &request.request.kind;
        assert_eq!(openai_request.target_tokens, Some(256));
        assert_eq!(
            openai_request.input_context.entry_ids(),
            vec![original_entry_id]
        );
        assert_eq!(openai_request.input_context.context_revision, 2);

        let completed = drive
            .resume_context_compaction(
                ContextCompactionResult {
                    session_id: request.session_id,
                    context_revision: openai_request.input_context.context_revision,
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
        let ContextCompactionRequestKind::OpenAiResponses(openai_request) = &request.request.kind;
        assert_eq!(openai_request.target_tokens, Some(4));
        assert_eq!(openai_request.input_context.entry_ids(), entry_ids);
        assert_eq!(
            openai_request
                .input_context
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
        let planned_request = active_turn.request.as_ref().expect("planned request");
        let LlmRequestKind::OpenAiResponses(openai) = &planned_request.kind else {
            panic!("expected OpenAI Responses request");
        };
        assert_eq!(openai.input_context.entries.len(), 1);
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
}
