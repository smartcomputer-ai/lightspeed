use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    BlobRef, ContextItemId, CoreAgentEventKind, CoreAgentEventProposal, CoreAgentJoins,
    CoreAgentState, CoreAgentStatus, DomainError, PlanNext, PlanningError, ProviderApiKind, RunId,
    RunStatus, SkillId, ToolCallId, ToolName, TurnId,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    ItemsRecorded {
        items: Vec<ContextItem>,
    },
    WindowPlanned {
        run_id: RunId,
        turn_id: TurnId,
        window: ContextWindow,
    },
    CompactionRecorded {
        run_id: RunId,
        turn_id: Option<TurnId>,
        record: CompactionRecord,
    },
}

pub type ContextEvent = Event;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextState {
    pub retained_items: Vec<ContextItem>,
    pub active_window: Option<ContextWindow>,
    pub latest_compaction: Option<CompactionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindow {
    pub api_kind: ProviderApiKind,
    pub item_ids: Vec<ContextItemId>,
    pub token_estimate: Option<TokenEstimate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedContextWindow {
    pub api_kind: ProviderApiKind,
    pub items: Vec<ContextItem>,
    pub token_estimate: Option<TokenEstimate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    pub item_id: ContextItemId,
    pub kind: ContextItemKind,
    pub source: ContextItemSource,
    pub native_item_ref: BlobRef,
    pub media_type: Option<String>,
    pub preview: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_item_id: Option<String>,
    pub token_estimate: Option<TokenEstimate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncommittedContextItem {
    pub kind: ContextItemKind,
    pub source: ContextItemSource,
    pub native_item_ref: BlobRef,
    pub media_type: Option<String>,
    pub preview: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_item_id: Option<String>,
    pub token_estimate: Option<TokenEstimate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemKind {
    Message { role: ContextMessageRole },
    SkillCatalog,
    SkillActivation { skill_id: SkillId },
    ToolCall { call_id: ToolCallId, name: ToolName },
    ToolResult { call_id: ToolCallId, is_error: bool },
    ReasoningState,
    CompactionState,
    ProviderOpaque,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemSource {
    Instructions,
    RunInput {
        run_id: RunId,
    },
    Steering {
        run_id: RunId,
    },
    AssistantOutput {
        run_id: RunId,
        turn_id: TurnId,
    },
    ToolCall {
        run_id: RunId,
        turn_id: TurnId,
    },
    ToolResult {
        run_id: RunId,
        turn_id: TurnId,
    },
    Reasoning {
        run_id: RunId,
        turn_id: TurnId,
    },
    Compaction {
        run_id: Option<RunId>,
        turn_id: Option<TurnId>,
    },
    Runtime {
        label: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenEstimate {
    pub tokens: u32,
    pub quality: TokenEstimateQuality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenEstimateQuality {
    Exact,
    ProviderCounted,
    Estimated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRecord {
    pub mode: CompactionMode,
    pub source_item_ids: Vec<ContextItemId>,
    pub output_item_ids: Vec<ContextItemId>,
    pub result_window: ContextWindow,
    pub summary_ref: Option<BlobRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMode {
    ProviderManaged,
    ProviderStandalone,
    ClientManaged,
}

pub(crate) fn resolve_context_window(
    state: &CoreAgentState,
    window: &ContextWindow,
) -> Result<ResolvedContextWindow, PlanningError> {
    let mut by_id = BTreeMap::<ContextItemId, ContextItem>::new();
    for item in &state.context.retained_items {
        by_id.insert(item.item_id, item.clone());
    }

    let items = window
        .item_ids
        .iter()
        .map(|item_id| {
            by_id.remove(item_id).ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "context window references missing item {}",
                    item_id
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ResolvedContextWindow {
        api_kind: window.api_kind.clone(),
        items,
        token_estimate: window.token_estimate.clone(),
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreContextPlanner;

impl PlanNext for CoreContextPlanner {
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
        if active_run.status != RunStatus::Active {
            return Ok(Vec::new());
        }

        if !state.context.retained_items.iter().any(|item| {
            matches!(
                item.source,
                ContextItemSource::RunInput { run_id } if run_id == active_run.run_id
            )
        }) {
            let next_item_id = state
                .id_cursors
                .last_context_item_id
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::InvariantViolation("context item id cursor exhausted".to_owned())
                })?;
            let item = ContextItem {
                item_id: ContextItemId::new(next_item_id),
                kind: ContextItemKind::Message {
                    role: ContextMessageRole::User,
                },
                source: ContextItemSource::RunInput {
                    run_id: active_run.run_id,
                },
                native_item_ref: active_run.input_ref.clone(),
                media_type: None,
                preview: None,
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            };
            let joins = CoreAgentJoins {
                run_id: Some(active_run.run_id),
                ..CoreAgentJoins::default()
            };
            return Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEventKind::Context(Event::ItemsRecorded { items: vec![item] }),
            )]);
        }

        let Some(turn_id) = active_run.active_turn_id else {
            return Ok(Vec::new());
        };
        if state.context.active_window.is_some() {
            return Ok(Vec::new());
        }

        let Some(config) = state.lifecycle.config.as_ref() else {
            return Err(DomainError::InvariantViolation(
                "active session is missing config".to_owned(),
            )
            .into());
        };
        let item_ids = state
            .context
            .retained_items
            .iter()
            .map(|item| item.item_id)
            .collect::<Vec<_>>();
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let joins = CoreAgentJoins {
            run_id: Some(active_run.run_id),
            turn_id: Some(turn_id),
            ..CoreAgentJoins::default()
        };
        let window = ContextWindow {
            api_kind: config.model.api_kind.clone(),
            item_ids,
            token_estimate: None,
        };

        Ok(vec![CoreAgentEventProposal::new(
            joins,
            CoreAgentEventKind::Context(Event::WindowPlanned {
                run_id: active_run.run_id,
                turn_id,
                window,
            }),
        )])
    }
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::ItemsRecorded { items } => {
            if items.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "context item event must contain at least one item".into(),
                ));
            }
            for item in items {
                let expected_item_id = state
                    .id_cursors
                    .last_context_item_id
                    .checked_add(1)
                    .ok_or_else(|| {
                        DomainError::InvariantViolation("context item id cursor exhausted".into())
                    })?;
                if item.item_id.as_u64() != expected_item_id {
                    return Err(DomainError::InvariantViolation(format!(
                        "expected context item id {}, got {}",
                        expected_item_id, item.item_id
                    )));
                }
                if state
                    .context
                    .retained_items
                    .iter()
                    .any(|existing| existing.item_id == item.item_id)
                {
                    return Err(DomainError::InvariantViolation(format!(
                        "duplicate context item id {}",
                        item.item_id
                    )));
                }
                state.context.retained_items.push(item.clone());
                state.id_cursors.last_context_item_id = item.item_id.as_u64();
            }
            state.context.active_window = None;
            Ok(())
        }
        Event::WindowPlanned {
            run_id,
            turn_id,
            window,
        } => {
            let active_run = crate::core::components::run::active_run_mut(state, *run_id)?;
            if active_run.active_turn_id != Some(*turn_id) {
                return Err(DomainError::InvariantViolation(
                    "context window turn does not match active turn".into(),
                ));
            }
            for item_id in &window.item_ids {
                if !state
                    .context
                    .retained_items
                    .iter()
                    .any(|item| item.item_id == *item_id)
                {
                    return Err(DomainError::InvariantViolation(format!(
                        "context window references unknown item {}",
                        item_id
                    )));
                }
            }
            state.context.active_window = Some(window.clone());
            Ok(())
        }
        Event::CompactionRecorded { record, .. } => {
            state.context.latest_compaction = Some(record.clone());
            state.context.active_window = Some(record.result_window.clone());
            Ok(())
        }
    }
}
