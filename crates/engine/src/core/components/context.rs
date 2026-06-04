use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BlobRef, ContextEntryKey, ContextItemId, CoreAgentEventKind, CoreAgentEventProposal,
    CoreAgentJoins, CoreAgentState, CoreAgentStatus, DomainError, PlanNext, PlanningError, RunId,
    RunStatus, SkillActivation, SkillId, ToolCallId, ToolName, TurnId,
};

const SESSION_INSTRUCTIONS_KEY: &str = "session.instructions";
const SKILL_CATALOG_KEY: &str = "skills.catalog";

pub type ContextEntryId = ContextItemId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// Applies new immutable entries to active context. Unkeyed entries append;
    /// keyed entries replace the previous active entry for that key.
    EntriesApplied { entries: Vec<ContextEntry> },
    /// Removes active context entries. The event log remains the durable audit
    /// history, so removed entries do not need to stay in reducer state.
    EntriesRemoved {
        entry_ids: Vec<ContextEntryId>,
        reason: ContextRemovalReason,
    },
    /// Removes replaceable active entries by key, such as cleared instructions.
    KeysRemoved { keys: Vec<ContextEntryKey> },
    /// Replaces the full active context state. This is intended for compaction
    /// and policy rewrites where the next active context is clearer as a whole
    /// state than as separate add/remove deltas.
    StateReplaced {
        entries: Vec<ContextEntry>,
        reason: ContextRewriteReason,
    },
}

pub type ContextEvent = Event;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextState {
    /// Active context entries in strictly increasing `entry_id` order. Gaps are
    /// expected after removals and compaction; ids are never reused.
    pub entries: Vec<ContextEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRemovalReason {
    Pruned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRewriteReason {
    Compacted {
        source_entries: Vec<ContextEntryId>,
        summary_ref: Option<BlobRef>,
    },
    Pruned,
    PolicyChanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    /// Immutable, session-local identity assigned by the reducer.
    pub entry_id: ContextEntryId,
    /// Optional live slot this entry replaces. The key is not identity; model
    /// requests, removals, and compaction should reference `entry_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<ContextEntryKey>,
    /// Provider-neutral semantic category used by planners, renderers, and projections.
    pub kind: ContextEntryKind,
    /// Provenance for deterministic planning, projection grouping, and audit.
    pub source: ContextEntrySource,
    /// CAS ref for the provider-native or Forge-native payload.
    pub content_ref: BlobRef,
    /// Optional MIME hint for renderers that need to distinguish text, JSON, files, etc.
    pub media_type: Option<String>,
    /// Short display text for projections and logs; not authoritative model input.
    pub preview: Option<String>,
    /// Provider-specific category for opaque/native entries that need round-tripping.
    pub provider_kind: Option<String>,
    /// Provider-assigned item id when preserving native conversation/state identity.
    pub provider_item_id: Option<String>,
    /// Optional accounting estimate used by context planning and compaction.
    pub token_estimate: Option<TokenEstimate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntryInput {
    pub kind: ContextEntryKind,
    pub content_ref: BlobRef,
    pub media_type: Option<String>,
    pub preview: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_item_id: Option<String>,
    pub token_estimate: Option<TokenEstimate>,
}

impl ContextEntryInput {
    fn commit(
        self,
        entry_id: ContextEntryId,
        key: Option<ContextEntryKey>,
        source: ContextEntrySource,
    ) -> ContextEntry {
        ContextEntry {
            entry_id,
            key,
            kind: self.kind,
            source,
            content_ref: self.content_ref,
            media_type: self.media_type,
            preview: self.preview,
            provider_kind: self.provider_kind,
            provider_item_id: self.provider_item_id,
            token_estimate: self.token_estimate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEntryKind {
    Message { role: ContextMessageRole },
    Instructions,
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
pub enum ContextEntrySource {
    ContextEdit,
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
    Tool {
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

pub(crate) fn planned_context_entry_ids(state: &CoreAgentState) -> Vec<ContextEntryId> {
    let mut entry_ids = Vec::new();
    let mut seen = BTreeSet::new();

    for key in [session_instructions_key(), skill_catalog_key()] {
        if let Some(entry) = current_key_entry(state, &key) {
            entry_ids.push(entry.entry_id);
            seen.insert(entry.entry_id);
        }
    }

    for entry in &state.context.entries {
        if seen.contains(&entry.entry_id) {
            continue;
        }

        match &entry.kind {
            ContextEntryKind::Instructions | ContextEntryKind::SkillCatalog => {}
            ContextEntryKind::SkillActivation { skill_id } => {
                if active_activation_for_entry(state, skill_id, &entry.content_ref).is_some() {
                    entry_ids.push(entry.entry_id);
                    seen.insert(entry.entry_id);
                }
            }
            _ => {
                entry_ids.push(entry.entry_id);
                seen.insert(entry.entry_id);
            }
        }
    }

    entry_ids
}

pub(crate) fn context_entries_by_id(
    state: &CoreAgentState,
    entry_ids: &[ContextEntryId],
) -> Result<Vec<ContextEntry>, PlanningError> {
    entry_ids
        .iter()
        .map(|entry_id| {
            entry_by_id(state, *entry_id).cloned().ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "context references missing entry {}",
                    entry_id
                ))
                .into()
            })
        })
        .collect()
}

pub(crate) fn context_entries_from_inputs(
    state: &CoreAgentState,
    inputs: Vec<(
        Option<ContextEntryKey>,
        ContextEntrySource,
        ContextEntryInput,
    )>,
) -> Result<Vec<ContextEntry>, DomainError> {
    let mut next_entry_id = state.id_cursors.last_context_item_id;
    inputs
        .into_iter()
        .map(|(key, source, entry)| {
            next_entry_id = next_entry_id.checked_add(1).ok_or_else(|| {
                DomainError::InvariantViolation("context entry id cursor exhausted".to_owned())
            })?;
            Ok(entry.commit(ContextEntryId::new(next_entry_id), key, source))
        })
        .collect()
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

        if let Some(proposal) = instruction_proposal(state, active_run.run_id)? {
            return Ok(vec![proposal]);
        }

        if let Some(proposal) = skill_catalog_proposal(state, active_run.run_id)? {
            return Ok(vec![proposal]);
        }

        let run_input_entries = missing_run_input_entries(state)?;
        if !run_input_entries.is_empty() {
            return Ok(vec![entries_applied_proposal(
                active_run.run_id,
                run_input_entries,
            )]);
        }

        let steering_entries = missing_steering_entries(state)?;
        if !steering_entries.is_empty() {
            return Ok(vec![entries_applied_proposal(
                active_run.run_id,
                steering_entries,
            )]);
        }

        let activation_entries = missing_direct_activation_entries(state)?;
        if !activation_entries.is_empty() {
            return Ok(vec![entries_applied_proposal(
                active_run.run_id,
                activation_entries,
            )]);
        }

        Ok(Vec::new())
    }
}

fn instruction_proposal(
    state: &CoreAgentState,
    run_id: RunId,
) -> Result<Option<CoreAgentEventProposal>, DomainError> {
    let key = session_instructions_key();
    let Some(config) = state.lifecycle.config.as_ref() else {
        return Err(DomainError::InvariantViolation(
            "open session is missing config".to_owned(),
        ));
    };

    let Some(instructions_ref) = config.context.instructions_ref.as_ref() else {
        if current_key_entry(state, &key).is_some() {
            return Ok(Some(keys_removed_proposal(run_id, vec![key])));
        }
        return Ok(None);
    };

    if current_key_entry(state, &key).is_some_and(|entry| entry.content_ref == *instructions_ref) {
        return Ok(None);
    }

    Ok(Some(entries_applied_proposal(
        run_id,
        vec![ContextEntry {
            entry_id: next_context_entry_id(state, 1)?,
            key: Some(key),
            kind: ContextEntryKind::Instructions,
            source: ContextEntrySource::Runtime {
                label: "session.instructions".to_owned(),
            },
            content_ref: instructions_ref.clone(),
            media_type: Some("text/plain".to_owned()),
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }],
    )))
}

fn skill_catalog_proposal(
    state: &CoreAgentState,
    run_id: RunId,
) -> Result<Option<CoreAgentEventProposal>, DomainError> {
    let key = skill_catalog_key();
    let Some(catalog) = state.skills.catalog.as_ref() else {
        if current_key_entry(state, &key).is_some() {
            return Ok(Some(keys_removed_proposal(run_id, vec![key])));
        }
        return Ok(None);
    };

    if current_key_entry(state, &key).is_some_and(|entry| entry.content_ref == catalog.catalog_ref)
    {
        return Ok(None);
    }

    Ok(Some(entries_applied_proposal(
        run_id,
        vec![ContextEntry {
            entry_id: next_context_entry_id(state, 1)?,
            key: Some(key),
            kind: ContextEntryKind::SkillCatalog,
            source: ContextEntrySource::Runtime {
                label: "skills.catalog".to_owned(),
            },
            content_ref: catalog.catalog_ref.clone(),
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }],
    )))
}

fn missing_run_input_entries(state: &CoreAgentState) -> Result<Vec<ContextEntry>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    if state.context.entries.iter().any(|entry| {
        matches!(
            entry.source,
            ContextEntrySource::RunInput { run_id } if run_id == active_run.run_id
        )
    }) {
        return Ok(Vec::new());
    }

    context_entries_from_inputs(
        state,
        active_run
            .input
            .iter()
            .cloned()
            .map(|entry| {
                (
                    None,
                    ContextEntrySource::RunInput {
                        run_id: active_run.run_id,
                    },
                    entry,
                )
            })
            .collect(),
    )
}

fn missing_steering_entries(state: &CoreAgentState) -> Result<Vec<ContextEntry>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };

    let recorded_entries_to_skip = state
        .context
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.source,
                ContextEntrySource::Steering { run_id } if run_id == active_run.run_id
            )
        })
        .count();

    context_entries_from_inputs(
        state,
        active_run
            .steering
            .iter()
            .flat_map(|input| input.iter())
            .skip(recorded_entries_to_skip)
            .cloned()
            .map(|entry| {
                (
                    None,
                    ContextEntrySource::Steering {
                        run_id: active_run.run_id,
                    },
                    entry,
                )
            })
            .collect(),
    )
}

fn missing_direct_activation_entries(
    state: &CoreAgentState,
) -> Result<Vec<ContextEntry>, DomainError> {
    let mut entries = Vec::new();
    for activation in &state.skills.activations {
        let Some(context_ref) = activation.direct_context_ref() else {
            continue;
        };
        if activation_context_is_active(state, activation) {
            continue;
        }

        entries.push(ContextEntry {
            entry_id: next_context_entry_id(state, entries.len() as u64 + 1)?,
            key: None,
            kind: ContextEntryKind::SkillActivation {
                skill_id: activation.skill_id.clone(),
            },
            source: ContextEntrySource::Runtime {
                label: "skills.activation".to_owned(),
            },
            content_ref: context_ref.clone(),
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        });
    }
    Ok(entries)
}

fn next_context_entry_id(
    state: &CoreAgentState,
    offset_from_next: u64,
) -> Result<ContextEntryId, DomainError> {
    let next_entry_id = state
        .id_cursors
        .last_context_item_id
        .checked_add(offset_from_next)
        .ok_or_else(|| {
            DomainError::InvariantViolation("context entry id cursor exhausted".to_owned())
        })?;
    Ok(ContextEntryId::new(next_entry_id))
}

fn activation_context_is_active(state: &CoreAgentState, activation: &SkillActivation) -> bool {
    let Some(context_ref) = activation.direct_context_ref() else {
        return true;
    };
    state.context.entries.iter().any(|entry| {
        &entry.content_ref == context_ref
            && match &entry.kind {
                ContextEntryKind::SkillActivation { skill_id } => skill_id == &activation.skill_id,
                ContextEntryKind::ToolResult { .. } => true,
                _ => false,
            }
    })
}

fn active_activation_for_entry<'a>(
    state: &'a CoreAgentState,
    skill_id: &SkillId,
    context_ref: &BlobRef,
) -> Option<&'a SkillActivation> {
    state.skills.activations.iter().find(|activation| {
        &activation.skill_id == skill_id && activation.direct_context_ref() == Some(context_ref)
    })
}

fn entry_by_id(state: &CoreAgentState, entry_id: ContextEntryId) -> Option<&ContextEntry> {
    state
        .context
        .entries
        .iter()
        .find(|entry| entry.entry_id == entry_id)
}

fn current_key_entry<'a>(
    state: &'a CoreAgentState,
    key: &ContextEntryKey,
) -> Option<&'a ContextEntry> {
    state
        .context
        .entries
        .iter()
        .find(|entry| entry.key.as_ref() == Some(key))
}

fn session_instructions_key() -> ContextEntryKey {
    ContextEntryKey::new(SESSION_INSTRUCTIONS_KEY)
}

fn skill_catalog_key() -> ContextEntryKey {
    ContextEntryKey::new(SKILL_CATALOG_KEY)
}

fn entries_applied_proposal(run_id: RunId, entries: Vec<ContextEntry>) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins {
            run_id: Some(run_id),
            ..CoreAgentJoins::default()
        },
        CoreAgentEventKind::Context(Event::EntriesApplied { entries }),
    )
}

fn keys_removed_proposal(run_id: RunId, keys: Vec<ContextEntryKey>) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins {
            run_id: Some(run_id),
            ..CoreAgentJoins::default()
        },
        CoreAgentEventKind::Context(Event::KeysRemoved { keys }),
    )
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::EntriesApplied { entries } => apply_entries_applied(state, entries),
        Event::EntriesRemoved { entry_ids, reason } => {
            validate_removal_reason(reason)?;
            remove_context_entries(state, entry_ids)
        }
        Event::KeysRemoved { keys } => {
            if keys.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "context key removal event must contain at least one key".into(),
                ));
            }
            for key in keys {
                remove_context_entry_by_key(state, key);
            }
            Ok(())
        }
        Event::StateReplaced { entries, reason } => replace_context_state(state, entries, reason),
    }
}

fn apply_entries_applied(
    state: &mut CoreAgentState,
    entries: &[ContextEntry],
) -> Result<(), DomainError> {
    if entries.is_empty() {
        return Err(DomainError::InvariantViolation(
            "context entries event must contain at least one entry".into(),
        ));
    }
    for entry in entries {
        let expected_entry_id = state
            .id_cursors
            .last_context_item_id
            .checked_add(1)
            .ok_or_else(|| {
                DomainError::InvariantViolation("context entry id cursor exhausted".into())
            })?;
        if entry.entry_id.as_u64() != expected_entry_id {
            return Err(DomainError::InvariantViolation(format!(
                "expected context entry id {}, got {}",
                expected_entry_id, entry.entry_id
            )));
        }
        if entry_by_id(state, entry.entry_id).is_some() {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate active context entry id {}",
                entry.entry_id
            )));
        }
        if let Some(last) = state.context.entries.last() {
            if entry.entry_id <= last.entry_id {
                return Err(DomainError::InvariantViolation(format!(
                    "context entry id {} must be greater than last active entry id {}",
                    entry.entry_id, last.entry_id
                )));
            }
        }

        if let Some(key) = entry.key.as_ref() {
            remove_context_entry_by_key(state, key);
        }

        state.context.entries.push(entry.clone());
        state.id_cursors.last_context_item_id = entry.entry_id.as_u64();
    }
    Ok(())
}

fn validate_removal_reason(reason: &ContextRemovalReason) -> Result<(), DomainError> {
    match reason {
        ContextRemovalReason::Pruned => Ok(()),
    }
}

fn remove_context_entries(
    state: &mut CoreAgentState,
    entry_ids: &[ContextEntryId],
) -> Result<(), DomainError> {
    if entry_ids.is_empty() {
        return Err(DomainError::InvariantViolation(
            "context entry removal event must contain at least one entry".into(),
        ));
    }

    let mut seen = BTreeSet::new();
    for entry_id in entry_ids {
        if !seen.insert(*entry_id) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate context entry removal {}",
                entry_id
            )));
        }
        if entry_by_id(state, *entry_id).is_none() {
            return Err(DomainError::InvariantViolation(format!(
                "cannot remove unknown context entry {}",
                entry_id
            )));
        }
    }

    state
        .context
        .entries
        .retain(|entry| !seen.contains(&entry.entry_id));
    Ok(())
}

fn remove_context_entry_by_key(state: &mut CoreAgentState, key: &ContextEntryKey) {
    state
        .context
        .entries
        .retain(|entry| entry.key.as_ref() != Some(key));
}

fn replace_context_state(
    state: &mut CoreAgentState,
    entries: &[ContextEntry],
    reason: &ContextRewriteReason,
) -> Result<(), DomainError> {
    validate_rewrite_reason(state, reason)?;
    validate_replacement_entries(state, entries)?;

    if let Some(last) = entries.last() {
        state.id_cursors.last_context_item_id = last
            .entry_id
            .as_u64()
            .max(state.id_cursors.last_context_item_id);
    }
    state.context.entries = entries.to_vec();
    Ok(())
}

fn validate_rewrite_reason(
    state: &CoreAgentState,
    reason: &ContextRewriteReason,
) -> Result<(), DomainError> {
    match reason {
        ContextRewriteReason::Compacted { source_entries, .. } => {
            if source_entries.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "context compaction rewrite must include source entries".into(),
                ));
            }
            let mut seen = BTreeSet::new();
            for entry_id in source_entries {
                if !seen.insert(*entry_id) {
                    return Err(DomainError::InvariantViolation(format!(
                        "duplicate context compaction source entry {}",
                        entry_id
                    )));
                }
                if entry_by_id(state, *entry_id).is_none() {
                    return Err(DomainError::InvariantViolation(format!(
                        "context compaction references unknown source entry {}",
                        entry_id
                    )));
                }
            }
            Ok(())
        }
        ContextRewriteReason::Pruned | ContextRewriteReason::PolicyChanged => Ok(()),
    }
}

fn validate_replacement_entries(
    state: &CoreAgentState,
    entries: &[ContextEntry],
) -> Result<(), DomainError> {
    let mut seen_ids = BTreeSet::new();
    let mut seen_keys = BTreeSet::new();
    let mut previous_entry_id = None;
    let mut next_new_entry_id = state
        .id_cursors
        .last_context_item_id
        .checked_add(1)
        .ok_or_else(|| {
            DomainError::InvariantViolation("context entry id cursor exhausted".into())
        })?;

    for entry in entries {
        if !seen_ids.insert(entry.entry_id) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate replacement context entry id {}",
                entry.entry_id
            )));
        }
        if let Some(previous_entry_id) = previous_entry_id {
            if entry.entry_id <= previous_entry_id {
                return Err(DomainError::InvariantViolation(format!(
                    "replacement context entry id {} must be greater than previous entry id {}",
                    entry.entry_id, previous_entry_id
                )));
            }
        }
        previous_entry_id = Some(entry.entry_id);

        if let Some(key) = entry.key.as_ref() {
            if !seen_keys.insert(key.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "duplicate replacement context key {}",
                    key
                )));
            }
        }

        match entry_by_id(state, entry.entry_id) {
            Some(existing) if existing != entry => {
                return Err(DomainError::InvariantViolation(format!(
                    "replacement context entry {} changes existing entry payload",
                    entry.entry_id
                )));
            }
            Some(_) => {}
            None => {
                if entry.entry_id.as_u64() != next_new_entry_id {
                    return Err(DomainError::InvariantViolation(format!(
                        "expected new replacement context entry id {}, got {}",
                        next_new_entry_id, entry.entry_id
                    )));
                }
                next_new_entry_id = next_new_entry_id.checked_add(1).ok_or_else(|| {
                    DomainError::InvariantViolation("context entry id cursor exhausted".into())
                })?;
            }
        }
    }

    Ok(())
}
