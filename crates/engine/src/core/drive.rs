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
    AdmitCommand, ApplyEvent, BlobRef, CodecError, CommandError, ContextEvent, ContextItemKind,
    ContextMessageRole, CoreAdmitCommand, CoreAgentCodec, CoreAgentEntry, CoreAgentEventKind,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus, CoreApplyEvent,
    CorePlanner, DomainError, LlmFinish, LlmGenerationRequest, LlmGenerationResult,
    LlmGenerationStatus, PlanNext, PlanningError, SessionId, SessionPosition, ToolCallResult,
    ToolCallStatus, ToolEvent, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationRequest, TurnEvent, TurnOutcome, UncommittedContextItem,
    core::components::context::context_items_from_uncommitted,
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
        BlobRef, CommandRejectionKind, ContextConfig, ContextConfigPatch, ContextItemSource,
        CoreAgentCommand, LlmGenerationFacts, LlmRequestKind, ModelProviderOptions, ModelSelection,
        OptionalConfigPatch, ProviderApiKind, ProviderRequestDefaults, RunConfig, RunFailureKind,
        RunStatus, SessionConfig, SessionConfigPatch, SkillActivation, SkillActivationScope,
        SkillActivationSource, SkillCatalogContext, SkillId, ToolCallId, TurnConfig,
        TurnConfigPatch,
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
            context: ContextConfig {
                instructions_ref: None,
                max_context_tokens: None,
                target_context_tokens: None,
                reserve_output_tokens: None,
                compaction_enabled: false,
            },
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

    fn open_session(drive: &mut CoreAgentDrive) {
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(drive, open);
    }

    fn request_run(drive: &mut CoreAgentDrive, input_ref: BlobRef) {
        let request = drive
            .admit_command(
                CoreAgentCommand::RequestRun {
                    submission_id: None,
                    input_ref,
                    run_config: run_config(),
                },
                20,
            )
            .expect("request run");
        commit_action(drive, request);
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

    fn openai_items(request: &LlmGenerationRequest) -> &[crate::ContextItem] {
        let LlmRequestKind::OpenAiResponses(openai) = &request.request.kind else {
            panic!("expected OpenAI Responses request");
        };
        &openai.input_window.items
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

        let instructions_ref = BlobRef::from_bytes(b"new instructions");
        let patch = SessionConfigPatch {
            turn: TurnConfigPatch {
                max_output_tokens: Some(OptionalConfigPatch::Set(2048)),
                ..TurnConfigPatch::default()
            },
            context: ContextConfigPatch {
                instructions_ref: Some(OptionalConfigPatch::Set(instructions_ref.clone())),
                compaction_enabled: Some(true),
                ..ContextConfigPatch::default()
            },
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
        assert_eq!(config.context.instructions_ref, Some(instructions_ref));
        assert!(config.context.compaction_enabled);
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
                    input_ref: BlobRef::from_bytes(b"input"),
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
    fn set_skill_activations_updates_state_without_starting_run() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        let open = drive
            .admit_command(CoreAgentCommand::OpenSession { config: config() }, 10)
            .expect("open");
        commit_action(&mut drive, open);

        let activation = SkillActivation {
            skill_id: SkillId::new("skill-1"),
            catalog_ref: BlobRef::from_bytes(b"catalog"),
            source: SkillActivationSource::DirectContext {
                context_ref: BlobRef::from_bytes(b"skill body"),
            },
            scope: SkillActivationScope::Run,
        };
        let action = drive
            .admit_command(
                CoreAgentCommand::SetSkillActivations {
                    activations: vec![activation.clone()],
                },
                20,
            )
            .expect("set skill activations");
        commit_action(&mut drive, action);

        assert_eq!(drive.state().skills.activations, vec![activation]);
        assert!(drive.state().runs.active.is_none());
        assert!(drive.state().runs.queued.is_empty());
        assert!(matches!(
            drive.next_action(30, 8).expect("next action"),
            CoreAgentAction::Idle
        ));
    }

    #[test]
    fn set_skill_activations_rejects_queued_work() {
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
                    input_ref: BlobRef::from_bytes(b"input"),
                    run_config: run_config(),
                },
                20,
            )
            .expect("request run");
        commit_action(&mut drive, request);

        let error = drive
            .admit_command(
                CoreAgentCommand::SetSkillActivations {
                    activations: Vec::new(),
                },
                30,
            )
            .expect_err("skill activations must reject queued work");

        let CoreAgentDriveError::Command(crate::CommandError::Rejected(rejection)) = error else {
            panic!("expected rejected command");
        };
        assert_eq!(rejection.kind, CommandRejectionKind::ActiveWork);
    }

    #[test]
    fn skill_catalog_and_direct_activation_are_planned_in_cache_preserving_order() {
        let session_id = SessionId::new("session-a");
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        open_session(&mut drive);

        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let set_catalog = drive
            .admit_command(
                CoreAgentCommand::SetSkillCatalog {
                    catalog: Some(SkillCatalogContext {
                        catalog_ref: catalog_ref.clone(),
                    }),
                },
                20,
            )
            .expect("set skill catalog");
        commit_action(&mut drive, set_catalog);

        let skill_id = SkillId::new("skill-1");
        let activation_ref = BlobRef::from_bytes(b"skill body");
        let activation = SkillActivation {
            skill_id: skill_id.clone(),
            catalog_ref: catalog_ref.clone(),
            source: SkillActivationSource::DirectContext {
                context_ref: activation_ref.clone(),
            },
            scope: SkillActivationScope::Run,
        };
        let set_activations = drive
            .admit_command(
                CoreAgentCommand::SetSkillActivations {
                    activations: vec![activation],
                },
                21,
            )
            .expect("set skill activations");
        commit_action(&mut drive, set_activations);

        let input_ref = BlobRef::from_bytes(b"input");
        request_run(&mut drive, input_ref.clone());

        let request = drive_until_generate(&mut drive);
        assert_eq!(request.session_id, session_id);
        let items = openai_items(&request);
        assert_eq!(items.len(), 3);
        assert!(matches!(items[0].kind, ContextItemKind::SkillCatalog));
        assert_eq!(items[0].native_item_ref, catalog_ref);
        assert!(matches!(
            items[1].kind,
            ContextItemKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(items[1].native_item_ref, input_ref);
        assert!(matches!(
            &items[2].kind,
            ContextItemKind::SkillActivation { skill_id: planned } if planned == &skill_id
        ));
        assert_eq!(items[2].native_item_ref, activation_ref);
    }

    #[test]
    fn tool_call_skill_activation_does_not_create_parallel_skill_context_item() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let activation = SkillActivation {
            skill_id: SkillId::new("skill-1"),
            catalog_ref: BlobRef::from_bytes(b"catalog"),
            source: SkillActivationSource::ToolResult {
                call_id: ToolCallId::new("call-1"),
            },
            scope: SkillActivationScope::Run,
        };
        let set_activations = drive
            .admit_command(
                CoreAgentCommand::SetSkillActivations {
                    activations: vec![activation],
                },
                20,
            )
            .expect("set skill activations");
        commit_action(&mut drive, set_activations);

        let input_ref = BlobRef::from_bytes(b"input");
        request_run(&mut drive, input_ref.clone());

        let request = drive_until_generate(&mut drive);
        let items = openai_items(&request);
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].kind,
            ContextItemKind::Message {
                role: ContextMessageRole::User
            }
        ));
        assert_eq!(items[0].native_item_ref, input_ref);
    }

    #[test]
    fn run_scoped_skill_activations_expire_when_run_completes() {
        let session_id = SessionId::new("session-a");
        let mut drive = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        open_session(&mut drive);

        let activation = SkillActivation {
            skill_id: SkillId::new("skill-1"),
            catalog_ref: BlobRef::from_bytes(b"catalog"),
            source: SkillActivationSource::DirectContext {
                context_ref: BlobRef::from_bytes(b"skill body"),
            },
            scope: SkillActivationScope::Run,
        };
        let set_activations = drive
            .admit_command(
                CoreAgentCommand::SetSkillActivations {
                    activations: vec![activation],
                },
                20,
            )
            .expect("set skill activations");
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
                    context_items: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: Some("resp-1".to_owned()),
                        finish: LlmFinish::Stop,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                        compaction: None,
                    },
                },
                30,
            )
            .expect("resume generation");
        commit_action(&mut drive, resumed);

        let complete_run = drive.next_action(31, 64).expect("complete run");
        commit_action(&mut drive, complete_run);

        assert!(drive.state().skills.activations.is_empty());

        request_run(&mut drive, BlobRef::from_bytes(b"next input"));
        let next_request = drive_until_generate(&mut drive);
        let next_items = openai_items(&next_request);
        assert!(
            next_items
                .iter()
                .all(|item| !matches!(item.kind, ContextItemKind::SkillActivation { .. }))
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
                    input_ref: BlobRef::from_bytes(b"input"),
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
                    input_ref: BlobRef::from_bytes(b"input"),
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
                    context_items: vec![UncommittedContextItem {
                        kind: ContextItemKind::Message {
                            role: ContextMessageRole::Assistant,
                        },
                        source: ContextItemSource::AssistantOutput {
                            run_id: request.run_id,
                            turn_id: request.turn_id,
                        },
                        native_item_ref: BlobRef::from_bytes(b"assistant output"),
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
                        compaction: None,
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
                    input_ref: BlobRef::from_bytes(b"input"),
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
                    context_items: Vec::new(),
                    facts: LlmGenerationFacts {
                        provider_response_id: None,
                        finish: LlmFinish::Failed,
                        usage: None,
                        tool_calls: Vec::new(),
                        context_token_estimate: None,
                        compaction: None,
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
