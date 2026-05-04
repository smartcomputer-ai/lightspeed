use serde::{Deserialize, Serialize};

use crate::{
    ActiveRun, BlobRef, CompactionRecord, CoreAgentEventKind, CoreAgentEventProposal,
    CoreAgentJoins, CoreAgentState, CoreAgentStatus, DomainError, LlmRequest, ObservedToolCall,
    PlanNext, PlanningError, RunId, RunStatus, TokenEstimate, TurnId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Started {
        turn_id: TurnId,
        run_id: RunId,
    },
    Planned {
        turn_id: TurnId,
        run_id: RunId,
        request: LlmRequest,
    },
    GenerationRequested {
        turn_id: TurnId,
        run_id: RunId,
    },
    GenerationCompleted {
        turn_id: TurnId,
        run_id: RunId,
        status: LlmGenerationStatus,
        facts: LlmGenerationFacts,
    },
    Completed {
        turn_id: TurnId,
        outcome: TurnOutcome,
    },
}

pub type TurnEvent = Event;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreTurnPlanner;

impl PlanNext for CoreTurnPlanner {
    fn plan_next(
        &self,
        state: &CoreAgentState,
    ) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
        if state.lifecycle.status != CoreAgentStatus::Open {
            return Ok(Vec::new());
        }

        let Some(active_run) = state.runs.active.as_ref() else {
            return Ok(Vec::new());
        };
        if active_run.active_tool_batch_id.is_some() {
            return Ok(Vec::new());
        }

        if let Some(turn_id) = active_run.active_turn_id {
            return decide_active_turn_progress(state, active_run, turn_id);
        }
        if active_run.status != RunStatus::Active {
            return Ok(Vec::new());
        }
        if crate::core_agent::components::run::latest_turn_is_terminal_run_outcome(active_run)? {
            return Ok(Vec::new());
        }

        let next_turn_id = state
            .id_cursors
            .last_turn_id
            .checked_add(1)
            .ok_or_else(|| {
                DomainError::InvariantViolation("turn id cursor exhausted".to_owned())
            })?;
        let turn_id = TurnId::new(next_turn_id);
        let joins = CoreAgentJoins {
            run_id: Some(active_run.run_id),
            turn_id: Some(turn_id),
            ..CoreAgentJoins::default()
        };
        let kind = CoreAgentEventKind::Turn(Event::Started {
            turn_id,
            run_id: active_run.run_id,
        });

        Ok(vec![CoreAgentEventProposal::new(joins, kind)])
    }
}

fn decide_active_turn_progress(
    state: &CoreAgentState,
    active_run: &ActiveRun,
    turn_id: TurnId,
) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    let Some(turn) = active_run.turns.get(&turn_id) else {
        return Err(DomainError::InvariantViolation(format!(
            "active turn {} is missing from run state",
            turn_id
        ))
        .into());
    };

    match turn.status {
        TurnStatus::Started => {
            if active_run.status != RunStatus::Active {
                return Ok(Vec::new());
            }
            let Some(window) = state.context.active_window.as_ref() else {
                return Ok(Vec::new());
            };
            let request = crate::core_agent::components::llm::build_llm_request(
                state, active_run, turn_id, window,
            )?;
            let joins = CoreAgentJoins {
                run_id: Some(active_run.run_id),
                turn_id: Some(turn_id),
                ..CoreAgentJoins::default()
            };
            Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEventKind::Turn(Event::Planned {
                    turn_id,
                    run_id: active_run.run_id,
                    request,
                }),
            )])
        }
        TurnStatus::Planned => {
            if active_run.status != RunStatus::Active {
                return Ok(Vec::new());
            }
            if turn.request.is_none() {
                return Err(DomainError::InvariantViolation(format!(
                    "planned turn {} is missing request",
                    turn_id
                ))
                .into());
            };
            let joins = CoreAgentJoins {
                run_id: Some(active_run.run_id),
                turn_id: Some(turn_id),
                ..CoreAgentJoins::default()
            };
            Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEventKind::Turn(Event::GenerationRequested {
                    turn_id,
                    run_id: active_run.run_id,
                }),
            )])
        }
        TurnStatus::GenerationPending => Ok(Vec::new()),
        TurnStatus::GenerationSettled
        | TurnStatus::Completed
        | TurnStatus::Failed
        | TurnStatus::Cancelled => Ok(Vec::new()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnState {
    pub turn_id: TurnId,
    pub run_id: RunId,
    pub status: TurnStatus,
    pub request: Option<LlmRequest>,
    pub generation_status: Option<LlmGenerationStatus>,
    pub facts: Option<LlmGenerationFacts>,
    pub outcome: Option<TurnOutcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Started,
    Planned,
    GenerationPending,
    GenerationSettled,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    FinalOutput { output_ref: Option<BlobRef> },
    ToolCallsQueued,
    ContextUpdateRequired,
    Failed { failure_ref: Option<BlobRef> },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGenerationFacts {
    pub provider_response_id: Option<String>,
    pub finish: LlmFinish,
    pub usage: Option<LlmUsage>,
    pub tool_calls: Vec<ObservedToolCall>,
    pub context_token_estimate: Option<TokenEstimate>,
    pub compaction: Option<CompactionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmGenerationStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFinish {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    ContextLimit,
    Cancelled,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmUsage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::Started { turn_id, run_id } => {
            let expected_turn_id =
                state
                    .id_cursors
                    .last_turn_id
                    .checked_add(1)
                    .ok_or_else(|| {
                        DomainError::InvariantViolation("turn id cursor exhausted".into())
                    })?;
            if turn_id.as_u64() != expected_turn_id {
                return Err(DomainError::InvariantViolation(format!(
                    "expected turn id {}, got {}",
                    expected_turn_id, turn_id
                )));
            }
            {
                let active_run =
                    crate::core_agent::components::run::active_run_mut(state, *run_id)?;
                if active_run.status != RunStatus::Active {
                    return Err(DomainError::InvariantViolation(
                        "turns can only start for active runs".into(),
                    ));
                }
                if active_run.active_turn_id.is_some() {
                    return Err(DomainError::InvariantViolation(
                        "cannot start turn while another turn is active".into(),
                    ));
                }
                active_run.turns.insert(
                    *turn_id,
                    TurnState {
                        turn_id: *turn_id,
                        run_id: *run_id,
                        status: TurnStatus::Started,
                        request: None,
                        generation_status: None,
                        facts: None,
                        outcome: None,
                    },
                );
                active_run.active_turn_id = Some(*turn_id);
            }
            state.id_cursors.last_turn_id = turn_id.as_u64();
            Ok(())
        }
        Event::Planned {
            turn_id,
            run_id,
            request,
        } => {
            crate::core_agent::components::llm::validate_request_matches_active_window(
                state, request,
            )?;
            let active_turn = active_turn_mut(state, *run_id, *turn_id)?;
            if active_turn.status != TurnStatus::Started {
                return Err(DomainError::InvariantViolation(
                    "turn can only be planned from started state".into(),
                ));
            }
            if active_turn.request.is_some() {
                return Err(DomainError::InvariantViolation(
                    "turn already has a planned request".into(),
                ));
            }
            active_turn.request = Some(request.clone());
            active_turn.status = TurnStatus::Planned;
            Ok(())
        }
        Event::GenerationRequested { turn_id, run_id } => {
            let active_turn = active_turn_mut(state, *run_id, *turn_id)?;
            if active_turn.status != TurnStatus::Planned {
                return Err(DomainError::InvariantViolation(
                    "generation can only be requested for planned turns".into(),
                ));
            }
            if active_turn.request.is_none() {
                return Err(DomainError::InvariantViolation(
                    "generation request requires a planned turn request".into(),
                ));
            }
            active_turn.status = TurnStatus::GenerationPending;
            Ok(())
        }
        Event::GenerationCompleted {
            turn_id,
            run_id,
            status,
            facts,
        } => {
            let active_turn = active_turn_mut(state, *run_id, *turn_id)?;
            if active_turn.status != TurnStatus::GenerationPending {
                return Err(DomainError::InvariantViolation(
                    "generation can only complete from pending state".into(),
                ));
            }
            if active_turn.facts.is_some() || active_turn.generation_status.is_some() {
                return Err(DomainError::InvariantViolation(
                    "turn already has a generation result".into(),
                ));
            }
            active_turn.generation_status = Some(status.clone());
            active_turn.facts = Some(facts.clone());
            active_turn.status = TurnStatus::GenerationSettled;
            Ok(())
        }
        Event::Completed { turn_id, outcome } => {
            let active_run = state
                .runs
                .active
                .as_mut()
                .ok_or_else(|| DomainError::InvariantViolation("no active run".into()))?;
            if active_run.active_turn_id != Some(*turn_id) {
                return Err(DomainError::InvariantViolation(
                    "completed turn does not match active turn".into(),
                ));
            }
            let turn = active_run.turns.get_mut(turn_id).ok_or_else(|| {
                DomainError::InvariantViolation(format!("active turn {} is missing", turn_id))
            })?;
            if turn.status != TurnStatus::GenerationSettled {
                return Err(DomainError::InvariantViolation(
                    "turn completion requires settled generation".into(),
                ));
            }
            let Some(status) = turn.generation_status.as_ref() else {
                return Err(DomainError::InvariantViolation(
                    "settled generation is missing status".into(),
                ));
            };
            let Some(facts) = turn.facts.as_ref() else {
                return Err(DomainError::InvariantViolation(
                    "settled generation is missing facts".into(),
                ));
            };
            validate_outcome_for_generation(status, facts, outcome)?;

            turn.outcome = Some(outcome.clone());
            turn.status = match outcome {
                TurnOutcome::FinalOutput { .. }
                | TurnOutcome::ToolCallsQueued
                | TurnOutcome::ContextUpdateRequired => TurnStatus::Completed,
                TurnOutcome::Failed { .. } => TurnStatus::Failed,
                TurnOutcome::Cancelled => TurnStatus::Cancelled,
            };
            active_run.active_turn_id = None;
            Ok(())
        }
    }
}

fn active_turn_mut(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
) -> Result<&mut TurnState, DomainError> {
    let active_run = crate::core_agent::components::run::active_run_mut(state, run_id)?;
    if active_run.active_turn_id != Some(turn_id) {
        return Err(DomainError::InvariantViolation(
            "event turn id does not match active turn".into(),
        ));
    }
    active_run.turns.get_mut(&turn_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active turn {} is missing", turn_id))
    })
}

fn validate_outcome_for_generation(
    status: &LlmGenerationStatus,
    facts: &LlmGenerationFacts,
    outcome: &TurnOutcome,
) -> Result<(), DomainError> {
    let valid = match status {
        LlmGenerationStatus::Cancelled => matches!(outcome, TurnOutcome::Cancelled),
        LlmGenerationStatus::Failed => matches!(outcome, TurnOutcome::Failed { .. }),
        LlmGenerationStatus::Succeeded => match facts.finish {
            LlmFinish::ToolCalls => matches!(outcome, TurnOutcome::ToolCallsQueued),
            LlmFinish::ContextLimit => matches!(outcome, TurnOutcome::ContextUpdateRequired),
            LlmFinish::Cancelled => matches!(outcome, TurnOutcome::Cancelled),
            LlmFinish::Failed => matches!(outcome, TurnOutcome::Failed { .. }),
            LlmFinish::Stop | LlmFinish::Length | LlmFinish::ContentFilter | LlmFinish::Unknown => {
                matches!(outcome, TurnOutcome::FinalOutput { .. })
            }
        },
    };
    if valid {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(
            "turn completion outcome does not match generation result".into(),
        ))
    }
}
