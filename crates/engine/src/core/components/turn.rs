use serde::{Deserialize, Serialize};

use crate::{
    ActiveRun, BlobRef, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, ObservedApprovalRequest, ObservedToolCall, PlanningError, RunId,
    RunStatus, TokenEstimate, TurnId,
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
        request_fingerprint: String,
        config_revision: u64,
        context_revision: u64,
        toolset_revision: u64,
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
    /// The run entered cancellation while this turn was still started,
    /// planned, or waiting for its generation. The turn ends
    /// `cancelled` without a generation result; any in-flight provider call
    /// is abandoned by the runtime and its result discarded.
    Cancelled {
        turn_id: TurnId,
        run_id: RunId,
    },
}

pub type TurnEvent = Event;

pub fn plan_next(state: &CoreAgentState) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
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
    if crate::core::components::run::latest_turn_is_terminal_run_outcome(active_run)? {
        return Ok(Vec::new());
    }

    let next_turn_id = state
        .id_cursors
        .last_turn_id
        .checked_add(1)
        .ok_or_else(|| DomainError::InvariantViolation("turn id cursor exhausted".to_owned()))?;
    let turn_id = TurnId::new(next_turn_id);
    let joins = CoreAgentJoins {
        run_id: Some(active_run.run_id),
        turn_id: Some(turn_id),
        ..CoreAgentJoins::default()
    };
    let kind = CoreAgentEvent::Turn(Event::Started {
        turn_id,
        run_id: active_run.run_id,
    });

    Ok(vec![CoreAgentEventProposal::new(joins, kind)])
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

    if active_run.status == RunStatus::Cancelling
        && matches!(
            turn.status,
            TurnStatus::Started | TurnStatus::Planned | TurnStatus::GenerationPending
        )
    {
        let joins = CoreAgentJoins {
            run_id: Some(active_run.run_id),
            turn_id: Some(turn_id),
            ..CoreAgentJoins::default()
        };
        return Ok(vec![CoreAgentEventProposal::new(
            joins,
            CoreAgentEvent::Turn(Event::Cancelled {
                turn_id,
                run_id: active_run.run_id,
            }),
        )]);
    }

    match turn.status {
        TurnStatus::Started => {
            if active_run.status != RunStatus::Active {
                return Ok(Vec::new());
            }
            let request =
                crate::core::components::llm::build_llm_request(state, active_run, turn_id)?;
            let joins = CoreAgentJoins {
                run_id: Some(active_run.run_id),
                turn_id: Some(turn_id),
                ..CoreAgentJoins::default()
            };
            Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEvent::Turn(Event::Planned {
                    turn_id,
                    run_id: active_run.run_id,
                    request_fingerprint: request.request_fingerprint,
                    config_revision: state.lifecycle.config_revision,
                    context_revision: request.context.context_revision,
                    toolset_revision: state.tooling.revision,
                }),
            )])
        }
        TurnStatus::Planned => {
            if active_run.status != RunStatus::Active {
                return Ok(Vec::new());
            }
            if turn.planned_request.is_none() {
                return Err(DomainError::InvariantViolation(format!(
                    "planned turn {} is missing request metadata",
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
                CoreAgentEvent::Turn(Event::GenerationRequested {
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
    pub planned_request: Option<PlannedRequestState>,
    pub generation_status: Option<LlmGenerationStatus>,
    pub facts: Option<LlmGenerationFacts>,
    pub outcome: Option<TurnOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRequestState {
    pub request_fingerprint: String,
    pub config_revision: u64,
    pub context_revision: u64,
    pub toolset_revision: u64,
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
    FinalOutput { output: Option<crate::ContentRef> },
    ToolCallsQueued,
    ContextUpdateRequired,
    ApprovalsRequested,
    Failed { failure_ref: Option<BlobRef> },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGenerationFacts {
    pub provider_response_id: Option<String>,
    pub finish: LlmFinish,
    pub usage: Option<LlmUsage>,
    /// Wall-clock milliseconds the executing runtime spent on this
    /// generation (provider round-trip measured around the client call).
    /// Recorded telemetry only — planning never branches on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub tool_calls: Vec<ObservedToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_requests: Vec<ObservedApprovalRequest>,
    pub context_token_estimate: Option<TokenEstimate>,
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
    /// Prompt tokens served from a provider cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
    /// Prompt tokens written into a provider cache on this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u32>,
    /// Explicit uncached prompt-token count when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss_input_tokens: Option<u32>,
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
                let active_run = crate::core::components::run::active_run_mut(state, *run_id)?;
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
                        planned_request: None,
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
            request_fingerprint,
            config_revision,
            context_revision,
            toolset_revision,
        } => {
            if *config_revision != state.lifecycle.config_revision {
                return Err(DomainError::InvariantViolation(format!(
                    "planned request config revision {} does not match active revision {}",
                    config_revision, state.lifecycle.config_revision
                )));
            }
            if *context_revision != state.context.revision {
                return Err(DomainError::InvariantViolation(format!(
                    "planned request context revision {} does not match active revision {}",
                    context_revision, state.context.revision
                )));
            }
            if *toolset_revision != state.tooling.revision {
                return Err(DomainError::InvariantViolation(format!(
                    "planned request toolset revision {} does not match active revision {}",
                    toolset_revision, state.tooling.revision
                )));
            }
            crate::core::components::context::mark_current_context_consumed_by_turn(
                state, *run_id, *turn_id,
            )?;
            let active_turn = active_turn_mut(state, *run_id, *turn_id)?;
            if active_turn.status != TurnStatus::Started {
                return Err(DomainError::InvariantViolation(
                    "turn can only be planned from started state".into(),
                ));
            }
            if active_turn.planned_request.is_some() {
                return Err(DomainError::InvariantViolation(
                    "turn already has planned request metadata".into(),
                ));
            }
            active_turn.planned_request = Some(PlannedRequestState {
                request_fingerprint: request_fingerprint.clone(),
                config_revision: *config_revision,
                context_revision: *context_revision,
                toolset_revision: *toolset_revision,
            });
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
            if active_turn.planned_request.is_none() {
                return Err(DomainError::InvariantViolation(
                    "generation request requires planned turn metadata".into(),
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
            {
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
            }
            if let Some(usage) = facts.usage.as_ref() {
                let run = crate::core::components::run::active_run_mut(state, *run_id)?;
                accumulate_usage(&mut run.usage, usage);
            }
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
                | TurnOutcome::ContextUpdateRequired
                | TurnOutcome::ApprovalsRequested => TurnStatus::Completed,
                TurnOutcome::Failed { .. } => TurnStatus::Failed,
                TurnOutcome::Cancelled => TurnStatus::Cancelled,
            };
            active_run.active_turn_id = None;
            Ok(())
        }
        Event::Cancelled { turn_id, run_id } => {
            let active_run = crate::core::components::run::active_run_mut(state, *run_id)?;
            if active_run.status != RunStatus::Cancelling {
                return Err(DomainError::InvariantViolation(
                    "turns can only be cancelled while their run is cancelling".into(),
                ));
            }
            if active_run.active_turn_id != Some(*turn_id) {
                return Err(DomainError::InvariantViolation(
                    "cancelled turn does not match active turn".into(),
                ));
            }
            let turn = active_run.turns.get_mut(turn_id).ok_or_else(|| {
                DomainError::InvariantViolation(format!("active turn {} is missing", turn_id))
            })?;
            if !matches!(
                turn.status,
                TurnStatus::Started | TurnStatus::Planned | TurnStatus::GenerationPending
            ) {
                return Err(DomainError::InvariantViolation(
                    "only started, planned, or generation-pending turns can be cancelled".into(),
                ));
            }
            turn.outcome = Some(TurnOutcome::Cancelled);
            turn.status = TurnStatus::Cancelled;
            active_run.active_turn_id = None;
            Ok(())
        }
    }
}

fn accumulate_usage(total: &mut Option<LlmUsage>, usage: &LlmUsage) {
    fn add(target: &mut Option<u32>, value: Option<u32>) {
        if let Some(value) = value {
            *target = Some(target.unwrap_or(0).saturating_add(value));
        }
    }
    let total = total.get_or_insert_with(|| LlmUsage {
        input_tokens: None,
        output_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
        cache_miss_input_tokens: None,
    });
    add(&mut total.input_tokens, usage.input_tokens);
    add(&mut total.output_tokens, usage.output_tokens);
    add(&mut total.reasoning_tokens, usage.reasoning_tokens);
    add(&mut total.total_tokens, usage.total_tokens);
    add(&mut total.cached_input_tokens, usage.cached_input_tokens);
    add(
        &mut total.cache_write_input_tokens,
        usage.cache_write_input_tokens,
    );
    add(
        &mut total.cache_miss_input_tokens,
        usage.cache_miss_input_tokens,
    );
}

fn active_turn_mut(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
) -> Result<&mut TurnState, DomainError> {
    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;
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
            LlmFinish::Failed | LlmFinish::ContentFilter | LlmFinish::Length => {
                matches!(outcome, TurnOutcome::Failed { .. })
            }
            LlmFinish::Stop | LlmFinish::Unknown => {
                if facts.approval_requests.is_empty() {
                    matches!(outcome, TurnOutcome::FinalOutput { .. })
                } else {
                    matches!(outcome, TurnOutcome::ApprovalsRequested)
                }
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
