use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BlobRef, CompactionPolicy, ContextEntryKey, ContextItemId, CoreAgentEventKind,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus, DomainError,
    PlanningError, ProviderApiKind, RunId, RunSource, RunSourceContextTrigger, RunStatus, SkillId,
    SteeringId, ToolBatchId, ToolCallId, ToolName, TurnId,
};

const RESERVED_RUN_CONTEXT_KEY_PREFIX: &str = "run";
const INSTRUCTIONS_KEY_PREFIX: &str = "instructions.";
pub const VFS_CATALOG_CONTEXT_KEY: &str = "environment.vfs_catalog";
pub const ENVIRONMENT_CATALOG_CONTEXT_KEY: &str = "environment.catalog";
pub const ENVIRONMENT_ACTIVE_CONTEXT_KEY: &str = "environment.active";
pub const SKILL_CATALOG_CONTEXT_KEY: &str = "skills.catalog";
pub const SKILL_ACTIVATION_CONTEXT_KEY_PREFIX: &str = "skills.activation.";
pub const SKILL_ACTIVATION_PROVIDER_KIND_RUN: &str = "lightspeed.skill.activation.run";
pub const SKILL_ACTIVATION_PROVIDER_KIND_SESSION: &str = "lightspeed.skill.activation.session";
pub const OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND: &str = "openai.responses.compaction";
pub const OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND: &str = "openai.responses.web_search_call";
pub const OPENAI_RESPONSES_MCP_LIST_TOOLS_PROVIDER_KIND: &str = "openai.responses.mcp_list_tools";
pub const OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND: &str = "openai.responses.mcp_call";
pub const OPENAI_RESPONSES_MCP_APPROVAL_REQUEST_PROVIDER_KIND: &str =
    "openai.responses.mcp_approval_request";
pub const ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND: &str = "anthropic.messages.compaction";
pub const ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND: &str =
    "anthropic.messages.server_tool_use";
pub const ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND: &str =
    "anthropic.messages.server_tool_result";
pub const ANTHROPIC_MESSAGES_MCP_TOOL_USE_PROVIDER_KIND: &str = "anthropic.messages.mcp_tool_use";
pub const ANTHROPIC_MESSAGES_MCP_TOOL_RESULT_PROVIDER_KIND: &str =
    "anthropic.messages.mcp_tool_result";

pub type ContextEntryId = ContextItemId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// Applies new immutable entries to active context. Unkeyed entries append;
    /// keyed entries replace the previous active entry for that key.
    EntriesApplied {
        base_revision: u64,
        entries: Vec<ContextEntry>,
    },
    /// Removes active context entries. The event log remains the durable audit
    /// history, so removed entries do not need to stay in reducer state.
    EntriesRemoved {
        base_revision: u64,
        entry_ids: Vec<ContextEntryId>,
        reason: ContextRemovalReason,
    },
    /// Removes replaceable active entries by key, such as cleared instructions.
    KeysRemoved {
        base_revision: u64,
        keys: Vec<ContextEntryKey>,
    },
    /// Atomically replaces every active keyed entry whose key starts with
    /// `key_prefix` with the supplied entries.
    KeyPrefixReplaced {
        base_revision: u64,
        key_prefix: ContextEntryKey,
        entries: Vec<ContextEntry>,
    },
    /// Replaces the full active context state for explicit prune or policy
    /// rewrites. Replacement entries must be active entries from the current
    /// state; new materialization uses `EntriesApplied`.
    StateReplaced {
        base_revision: u64,
        entries: Vec<ContextEntry>,
        reason: ContextRewriteReason,
    },
    CompactionRequested {
        base_revision: u64,
        trigger: ContextCompactionTrigger,
    },
    CompactionFinished {
        base_revision: u64,
        status: ContextCompactionStatus,
        failure_ref: Option<BlobRef>,
    },
}

pub type ContextEvent = Event;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextState {
    /// Monotonic active-context revision used to guard rewrites and turn snapshots.
    pub revision: u64,
    /// Active context entries in strictly increasing `entry_id` order. Gaps are
    /// expected after removals and state rewrites; ids are never reused.
    pub entries: Vec<ContextEntry>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pending_compaction: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub api_kind: ProviderApiKind,
    pub context_revision: u64,
    pub entries: Vec<ContextEntry>,
    pub token_estimate: Option<TokenEstimate>,
}

impl ContextSnapshot {
    pub fn entry_ids(&self) -> Vec<ContextEntryId> {
        self.entries.iter().map(|entry| entry.entry_id).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRemovalReason {
    Pruned,
    ProviderCompacted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRewriteReason {
    Pruned,
    PolicyChanged,
    ProviderCompacted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionTrigger {
    Manual,
    HighWatermark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionStatus {
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    /// Immutable, session-local identity assigned by the reducer.
    pub entry_id: ContextEntryId,
    /// Optional live slot this entry replaces. The key is not identity; model
    /// requests, removals, and rewrites should reference `entry_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<ContextEntryKey>,
    /// Provider-neutral semantic category used by planners, renderers, and projections.
    pub kind: ContextEntryKind,
    /// Provenance for deterministic planning, projection grouping, and audit.
    pub source: ContextEntrySource,
    /// CAS ref for the provider-native or Lightspeed-native payload.
    pub content_ref: BlobRef,
    /// Optional MIME hint for renderers that need to distinguish text, JSON, files, etc.
    pub media_type: Option<String>,
    /// Short display text for projections and logs; not authoritative model input.
    pub preview: Option<String>,
    /// Provider-specific category for opaque/native entries that need round-tripping.
    pub provider_kind: Option<String>,
    /// Provider-assigned item id when preserving native conversation/state identity.
    pub provider_item_id: Option<String>,
    /// Optional accounting estimate used by context planning.
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
    VfsCatalog,
    EnvironmentCatalog,
    EnvironmentActive,
    SkillCatalog,
    SkillActivation { skill_id: SkillId },
    ToolCall { call_id: ToolCallId, name: ToolName },
    ToolResult { call_id: ToolCallId, is_error: bool },
    ReasoningState,
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
        input_index: u32,
    },
    Steering {
        run_id: RunId,
        steering_id: SteeringId,
        input_index: u32,
    },
    AssistantOutput {
        run_id: RunId,
        turn_id: TurnId,
    },
    Tool {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: Option<ToolBatchId>,
    },
    Reasoning {
        run_id: RunId,
        turn_id: TurnId,
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

    let mut instruction_entries = state
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::Instructions))
        .collect::<Vec<_>>();
    instruction_entries.sort_by(|left, right| {
        left.key
            .cmp(&right.key)
            .then_with(|| left.entry_id.cmp(&right.entry_id))
    });
    for entry in instruction_entries {
        entry_ids.push(entry.entry_id);
        seen.insert(entry.entry_id);
    }

    if let Some(entry) = current_key_entry(state, &skill_catalog_key()) {
        entry_ids.push(entry.entry_id);
        seen.insert(entry.entry_id);
    }
    for key in [
        vfs_catalog_key(),
        environment_catalog_key(),
        environment_active_key(),
    ] {
        if let Some(entry) = current_key_entry(state, &key)
            && seen.insert(entry.entry_id)
        {
            entry_ids.push(entry.entry_id);
        }
    }

    for entry in &state.context.entries {
        if seen.contains(&entry.entry_id) {
            continue;
        }

        match &entry.kind {
            ContextEntryKind::Instructions
            | ContextEntryKind::SkillCatalog
            | ContextEntryKind::VfsCatalog
            | ContextEntryKind::EnvironmentCatalog
            | ContextEntryKind::EnvironmentActive => {}
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

pub(crate) fn planned_context_snapshot(
    state: &CoreAgentState,
    api_kind: ProviderApiKind,
) -> Result<ContextSnapshot, PlanningError> {
    let entry_ids = planned_context_entry_ids(state);
    let entries = context_entries_by_id(state, &entry_ids)?;
    Ok(ContextSnapshot {
        api_kind,
        context_revision: state.context.revision,
        token_estimate: combined_token_estimate(&entries),
        entries,
    })
}

pub(crate) fn compactable_context_entry_ids(state: &CoreAgentState) -> Vec<ContextEntryId> {
    planned_context_entry_ids(state)
        .into_iter()
        .filter(|entry_id| {
            entry_by_id(state, *entry_id).is_some_and(|entry| {
                !matches!(
                    entry.kind,
                    ContextEntryKind::Instructions
                        | ContextEntryKind::SkillCatalog
                        | ContextEntryKind::SkillActivation { .. }
                        | ContextEntryKind::VfsCatalog
                        | ContextEntryKind::EnvironmentCatalog
                        | ContextEntryKind::EnvironmentActive
                )
            })
        })
        .collect()
}

pub(crate) fn compactable_context_snapshot(
    state: &CoreAgentState,
    api_kind: ProviderApiKind,
) -> Result<ContextSnapshot, PlanningError> {
    let entry_ids = compactable_context_entry_ids(state);
    if entry_ids.is_empty() {
        return Err(DomainError::InvariantViolation(
            "no compactable context entries are active".to_owned(),
        )
        .into());
    }
    let entries = context_entries_by_id(state, &entry_ids)?;
    Ok(ContextSnapshot {
        api_kind,
        context_revision: state.context.revision,
        token_estimate: combined_token_estimate(&entries),
        entries,
    })
}

pub(crate) fn mark_current_context_consumed_by_turn(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
) -> Result<(), DomainError> {
    let planned_ids = planned_context_entry_ids(state).into_iter().collect();
    mark_context_entries_consumed_by_turn(state, run_id, turn_id, planned_ids)
}

fn mark_context_entries_consumed_by_turn(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    consumed_ids: BTreeSet<ContextEntryId>,
) -> Result<(), DomainError> {
    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;

    if active_run.input_consumed_by_turn_id.is_none()
        && active_run
            .input_entry_ids
            .iter()
            .all(|entry_id| consumed_ids.contains(entry_id))
    {
        active_run.input_consumed_by_turn_id = Some(turn_id);
    }

    for steering in &mut active_run.steering {
        if steering.consumed_by_turn_id.is_none()
            && steering
                .entry_ids
                .iter()
                .all(|entry_id| consumed_ids.contains(entry_id))
        {
            steering.consumed_by_turn_id = Some(turn_id);
        }
    }

    Ok(())
}

fn combined_token_estimate(entries: &[ContextEntry]) -> Option<TokenEstimate> {
    let mut tokens = 0u32;
    let mut quality = TokenEstimateQuality::Exact;
    for entry in entries {
        let estimate = entry.token_estimate.as_ref()?;
        tokens = tokens.checked_add(estimate.tokens)?;
        quality = match (quality, estimate.quality) {
            (TokenEstimateQuality::Estimated, _) | (_, TokenEstimateQuality::Estimated) => {
                TokenEstimateQuality::Estimated
            }
            (TokenEstimateQuality::ProviderCounted, _)
            | (_, TokenEstimateQuality::ProviderCounted) => TokenEstimateQuality::ProviderCounted,
            (TokenEstimateQuality::Exact, TokenEstimateQuality::Exact) => {
                TokenEstimateQuality::Exact
            }
        };
    }
    Some(TokenEstimate { tokens, quality })
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

pub(crate) fn validate_external_context_edit(
    key: &ContextEntryKey,
    entry: &ContextEntryInput,
) -> Result<(), DomainError> {
    validate_external_context_key(key)?;
    validate_external_context_edit_entry(key, entry)
}

pub(crate) fn validate_external_context_prefix_replacement(
    key_prefix: &ContextEntryKey,
    entries: &std::collections::BTreeMap<ContextEntryKey, ContextEntryInput>,
) -> Result<(), DomainError> {
    validate_external_context_key(key_prefix)?;
    for (key, entry) in entries {
        validate_external_context_key(key)?;
        if !context_key_starts_with(key, key_prefix) {
            return Err(DomainError::InvariantViolation(format!(
                "context replacement entry key {} is outside prefix {}",
                key, key_prefix
            )));
        }
        validate_external_context_edit_entry(key, entry)?;
    }
    Ok(())
}

pub fn validate_external_context_key(key: &ContextEntryKey) -> Result<(), DomainError> {
    if key.as_str() == RESERVED_RUN_CONTEXT_KEY_PREFIX
        || key
            .as_str()
            .strip_prefix(RESERVED_RUN_CONTEXT_KEY_PREFIX)
            .is_some_and(|suffix| suffix.starts_with('.'))
    {
        return Err(DomainError::InvariantViolation(format!(
            "context key {} uses reserved internal prefix {}",
            key, RESERVED_RUN_CONTEXT_KEY_PREFIX
        )));
    }
    Ok(())
}

pub(crate) fn context_prefix_replacement_is_noop(
    state: &CoreAgentState,
    key_prefix: &ContextEntryKey,
    entries: &std::collections::BTreeMap<ContextEntryKey, ContextEntryInput>,
) -> bool {
    let active = state
        .context
        .entries
        .iter()
        .filter_map(|entry| {
            let key = entry.key.as_ref()?;
            if context_key_starts_with(key, key_prefix) {
                Some((key.clone(), context_entry_input_from_active(entry)))
            } else {
                None
            }
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    active == *entries
}

pub(crate) fn context_upsert_is_noop(
    state: &CoreAgentState,
    key: &ContextEntryKey,
    entry: &ContextEntryInput,
) -> bool {
    current_key_entry(state, key)
        .map(|active| context_entry_input_from_active(active) == *entry)
        .unwrap_or(false)
}

pub(crate) fn validate_context_key_exists(
    state: &CoreAgentState,
    key: &ContextEntryKey,
) -> Result<(), DomainError> {
    if current_key_entry(state, key).is_some() {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "context key {} does not exist",
            key
        )))
    }
}

pub(crate) fn validate_run_trigger_context_keys(
    state: &CoreAgentState,
    keys: &[ContextEntryKey],
) -> Result<Vec<RunSourceContextTrigger>, DomainError> {
    if keys.is_empty() {
        return Err(DomainError::InvariantViolation(
            "run trigger context keys must not be empty".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut triggers = Vec::with_capacity(keys.len());
    for key in keys {
        if !seen.insert(key.clone()) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate run trigger context key: {key}"
            )));
        }
        let Some(entry) = current_key_entry(state, key) else {
            return Err(DomainError::InvariantViolation(format!(
                "run trigger context key {key} does not exist"
            )));
        };
        triggers.push(RunSourceContextTrigger {
            key: key.clone(),
            entry_id: entry.entry_id,
        });
    }
    Ok(triggers)
}

pub(crate) fn run_input_context_keys(
    run_id: RunId,
    input_len: usize,
) -> Result<Vec<ContextEntryKey>, DomainError> {
    (0..input_len)
        .map(|index| {
            let index = input_index(index)?;
            Ok(ContextEntryKey::new(format!(
                "run.{}.input.{index}",
                run_id.as_u64()
            )))
        })
        .collect()
}

pub(crate) fn validate_run_input_entries(entries: &[ContextEntryInput]) -> Result<(), DomainError> {
    for entry in entries {
        validate_run_supplied_context_entry(entry, "run input")?;
    }
    Ok(())
}

pub(crate) fn validate_steering_input_entries(
    entries: &[ContextEntryInput],
) -> Result<(), DomainError> {
    for entry in entries {
        validate_run_supplied_context_entry(entry, "run steering")?;
    }
    Ok(())
}

fn validate_run_supplied_context_entry(
    entry: &ContextEntryInput,
    source: &'static str,
) -> Result<(), DomainError> {
    match &entry.kind {
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        }
        | ContextEntryKind::ProviderOpaque => Ok(()),
        _ => Err(DomainError::InvariantViolation(format!(
            "{} cannot supply context entry kind {:?}",
            source, entry.kind
        ))),
    }
}

fn validate_external_context_edit_entry(
    key: &ContextEntryKey,
    entry: &ContextEntryInput,
) -> Result<(), DomainError> {
    if is_instructions_key(key) {
        return match &entry.kind {
            ContextEntryKind::Instructions => Ok(()),
            _ => Err(DomainError::InvariantViolation(format!(
                "instruction context key {} cannot supply context entry kind {:?}",
                key, entry.kind
            ))),
        };
    }

    if key.as_str() == SKILL_CATALOG_CONTEXT_KEY {
        return match &entry.kind {
            ContextEntryKind::SkillCatalog => Ok(()),
            _ => Err(DomainError::InvariantViolation(format!(
                "skill catalog context key {} cannot supply context entry kind {:?}",
                key, entry.kind
            ))),
        };
    }

    if key.as_str() == VFS_CATALOG_CONTEXT_KEY {
        return match &entry.kind {
            ContextEntryKind::VfsCatalog => Ok(()),
            _ => Err(DomainError::InvariantViolation(format!(
                "VFS catalog context key {} cannot supply context entry kind {:?}",
                key, entry.kind
            ))),
        };
    }

    if key.as_str() == ENVIRONMENT_CATALOG_CONTEXT_KEY {
        return match &entry.kind {
            ContextEntryKind::EnvironmentCatalog => Ok(()),
            _ => Err(DomainError::InvariantViolation(format!(
                "environment catalog context key {} cannot supply context entry kind {:?}",
                key, entry.kind
            ))),
        };
    }

    if key.as_str() == ENVIRONMENT_ACTIVE_CONTEXT_KEY {
        return match &entry.kind {
            ContextEntryKind::EnvironmentActive => Ok(()),
            _ => Err(DomainError::InvariantViolation(format!(
                "active environment context key {} cannot supply context entry kind {:?}",
                key, entry.kind
            ))),
        };
    }

    if let Some(skill_id) = key
        .as_str()
        .strip_prefix(SKILL_ACTIVATION_CONTEXT_KEY_PREFIX)
    {
        return match &entry.kind {
            ContextEntryKind::SkillActivation {
                skill_id: entry_skill_id,
            } if entry_skill_id.as_str() == skill_id => Ok(()),
            ContextEntryKind::SkillActivation { skill_id } => {
                Err(DomainError::InvariantViolation(format!(
                    "skill activation context key {} does not match entry skill id {}",
                    key, skill_id
                )))
            }
            _ => Err(DomainError::InvariantViolation(format!(
                "skill activation context key {} cannot supply context entry kind {:?}",
                key, entry.kind
            ))),
        };
    }

    match &entry.kind {
        ContextEntryKind::ProviderOpaque => Ok(()),
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        } => Ok(()),
        ContextEntryKind::Instructions => Err(DomainError::InvariantViolation(format!(
            "instruction context entry requires an {}* key, got {}",
            INSTRUCTIONS_KEY_PREFIX, key
        ))),
        ContextEntryKind::VfsCatalog => Err(DomainError::InvariantViolation(format!(
            "VFS catalog context entry requires key {}, got {}",
            VFS_CATALOG_CONTEXT_KEY, key
        ))),
        ContextEntryKind::EnvironmentCatalog => Err(DomainError::InvariantViolation(format!(
            "environment catalog context entry requires key {}, got {}",
            ENVIRONMENT_CATALOG_CONTEXT_KEY, key
        ))),
        ContextEntryKind::EnvironmentActive => Err(DomainError::InvariantViolation(format!(
            "active environment context entry requires key {}, got {}",
            ENVIRONMENT_ACTIVE_CONTEXT_KEY, key
        ))),
        _ => Err(DomainError::InvariantViolation(format!(
            "context edit cannot supply context entry kind {:?}",
            entry.kind
        ))),
    }
}

fn is_instructions_key(key: &ContextEntryKey) -> bool {
    key.as_str().starts_with(INSTRUCTIONS_KEY_PREFIX)
}

pub fn plan_next(state: &CoreAgentState) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    if state.lifecycle.status != CoreAgentStatus::Open {
        return Ok(Vec::new());
    }

    if let Some(proposal) = provider_compacted_prune_proposal(state)? {
        return Ok(vec![proposal]);
    }

    if let Some(proposal) = high_watermark_compaction_proposal(state)? {
        return Ok(vec![proposal]);
    }

    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    if active_run.status != RunStatus::Active {
        return Ok(Vec::new());
    }

    let run_input_entries = missing_run_input_entries(state)?;
    if !run_input_entries.is_empty() {
        return Ok(vec![entries_applied_proposal(
            state,
            active_run.run_id,
            run_input_entries,
        )]);
    }

    let steering_entries = missing_steering_entries(state)?;
    if !steering_entries.is_empty() {
        return Ok(vec![entries_applied_proposal(
            state,
            active_run.run_id,
            steering_entries,
        )]);
    }

    Ok(Vec::new())
}

pub(crate) fn manual_compaction_requested_proposal(
    state: &CoreAgentState,
) -> Result<CoreAgentEventProposal, DomainError> {
    validate_standalone_compaction_can_start(state)?;
    if compactable_context_entry_ids(state).is_empty() {
        return Err(DomainError::InvariantViolation(
            "no compactable context entries are active".to_owned(),
        ));
    }
    Ok(compaction_requested_proposal(
        state,
        ContextCompactionTrigger::Manual,
    ))
}

fn high_watermark_compaction_proposal(
    state: &CoreAgentState,
) -> Result<Option<CoreAgentEventProposal>, DomainError> {
    if state.context.pending_compaction || state.runs.active.is_some() {
        return Ok(None);
    }
    if !state.runs.queued.is_empty() {
        return Ok(None);
    }
    let Some(config) = state.lifecycle.config.as_ref() else {
        return Ok(None);
    };
    let Some(CompactionPolicy::ProviderStandalone {
        compact_threshold_tokens: Some(compact_threshold_tokens),
        ..
    }) = &config.context.compaction
    else {
        return Ok(None);
    };
    if compactable_context_entry_ids(state).is_empty() {
        return Ok(None);
    }
    let snapshot = compactable_context_snapshot(state, config.model.api_kind.clone())
        .map_err(|error| DomainError::InvariantViolation(error.to_string()))?;
    let Some(estimate) = snapshot.token_estimate else {
        return Ok(None);
    };
    if estimate.tokens < *compact_threshold_tokens {
        return Ok(None);
    }
    Ok(Some(compaction_requested_proposal(
        state,
        ContextCompactionTrigger::HighWatermark,
    )))
}

fn compaction_requested_proposal(
    state: &CoreAgentState,
    trigger: ContextCompactionTrigger,
) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEventKind::Context(Event::CompactionRequested {
            base_revision: state.context.revision,
            trigger,
        }),
    )
}

pub(crate) fn validate_standalone_compaction_can_start(
    state: &CoreAgentState,
) -> Result<(), DomainError> {
    let Some(config) = state.lifecycle.config.as_ref() else {
        return Err(DomainError::InvariantViolation(
            "open session is missing config".to_owned(),
        ));
    };
    if !matches!(
        config.context.compaction,
        Some(CompactionPolicy::ProviderStandalone { .. })
    ) {
        return Err(DomainError::ProviderCompatibility(
            "context compaction command requires provider-standalone compaction policy".to_owned(),
        ));
    }
    if state.context.pending_compaction {
        return Err(DomainError::InvariantViolation(
            "context compaction is already pending".to_owned(),
        ));
    }
    if state.runs.active.is_some() || !state.runs.queued.is_empty() {
        return Err(DomainError::InvariantViolation(
            "context compaction can only run while no run is active or queued".to_owned(),
        ));
    }
    Ok(())
}

fn missing_run_input_entries(state: &CoreAgentState) -> Result<Vec<ContextEntry>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    let RunSource::Input { input } = &active_run.source else {
        return Ok(Vec::new());
    };
    if active_run.input_entry_ids.len() >= input.len() {
        return Ok(Vec::new());
    }

    let keys = run_input_context_keys(active_run.run_id, input.len())?;
    context_entries_from_inputs(
        state,
        input
            .iter()
            .enumerate()
            .skip(active_run.input_entry_ids.len())
            .map(|(index, entry)| {
                let input_index = input_index(index)?;
                Ok((
                    Some(keys[index].clone()),
                    ContextEntrySource::RunInput {
                        run_id: active_run.run_id,
                        input_index,
                    },
                    entry.clone(),
                ))
            })
            .collect::<Result<Vec<_>, DomainError>>()?,
    )
}

fn missing_steering_entries(state: &CoreAgentState) -> Result<Vec<ContextEntry>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };

    let mut inputs = Vec::new();
    for steering in &active_run.steering {
        if steering.entry_ids.len() >= steering.input.len() {
            continue;
        }
        for (index, entry) in steering
            .input
            .iter()
            .enumerate()
            .skip(steering.entry_ids.len())
        {
            inputs.push((
                None,
                ContextEntrySource::Steering {
                    run_id: active_run.run_id,
                    steering_id: steering.steering_id,
                    input_index: input_index(index)?,
                },
                entry.clone(),
            ));
        }
    }

    context_entries_from_inputs(state, inputs)
}

fn input_index(index: usize) -> Result<u32, DomainError> {
    index.try_into().map_err(|_| {
        DomainError::InvariantViolation(format!("context input index {} exceeds u32", index))
    })
}

fn provider_compacted_prune_proposal(
    state: &CoreAgentState,
) -> Result<Option<CoreAgentEventProposal>, DomainError> {
    if has_active_nonterminal_tool_batch(state) {
        return Ok(None);
    }

    let Some(latest_compaction_entry) = latest_provider_compaction_entry(state) else {
        return Ok(None);
    };
    let entry_ids = state
        .context
        .entries
        .iter()
        .filter(|entry| entry.entry_id < latest_compaction_entry.entry_id)
        .filter(|entry| is_provider_compaction_prunable_entry(state, entry))
        .map(|entry| entry.entry_id)
        .collect::<Vec<_>>();
    if entry_ids.is_empty() {
        return Ok(None);
    }

    Ok(Some(CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEventKind::Context(Event::EntriesRemoved {
            base_revision: state.context.revision,
            entry_ids,
            reason: ContextRemovalReason::ProviderCompacted,
        }),
    )))
}

fn latest_provider_compaction_entry(state: &CoreAgentState) -> Option<&ContextEntry> {
    state
        .context
        .entries
        .iter()
        .rev()
        .find(|entry| is_provider_compaction_entry(entry))
}

fn is_provider_compaction_entry(entry: &ContextEntry) -> bool {
    match entry.provider_kind.as_deref() {
        // OpenAI Responses returns an opaque encrypted compaction item.
        Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND) => {
            matches!(entry.kind, ContextEntryKind::ProviderOpaque)
        }
        // The Anthropic adapter compacts by summarization and returns the
        // summary as a user-visible replacement message.
        Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND) => {
            matches!(entry.kind, ContextEntryKind::Message { .. })
        }
        _ => false,
    }
}

fn is_provider_compaction_prunable_entry(state: &CoreAgentState, entry: &ContextEntry) -> bool {
    if validate_entry_is_not_unconsumed_active_run_input(state, entry.entry_id).is_err() {
        return false;
    }
    match entry.kind {
        ContextEntryKind::Instructions
        | ContextEntryKind::SkillCatalog
        | ContextEntryKind::SkillActivation { .. }
        | ContextEntryKind::VfsCatalog
        | ContextEntryKind::EnvironmentCatalog
        | ContextEntryKind::EnvironmentActive => false,
        ContextEntryKind::Message { .. }
        | ContextEntryKind::ToolCall { .. }
        | ContextEntryKind::ToolResult { .. }
        | ContextEntryKind::ReasoningState
        | ContextEntryKind::ProviderOpaque => true,
    }
}

fn has_active_nonterminal_tool_batch(state: &CoreAgentState) -> bool {
    state.runs.active.as_ref().is_some_and(|active_run| {
        active_run
            .tool_batches
            .values()
            .any(|batch| batch.calls.iter().any(|call| !call.status.is_terminal()))
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

fn skill_catalog_key() -> ContextEntryKey {
    ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY)
}

fn vfs_catalog_key() -> ContextEntryKey {
    ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY)
}

fn environment_catalog_key() -> ContextEntryKey {
    ContextEntryKey::new(ENVIRONMENT_CATALOG_CONTEXT_KEY)
}

fn environment_active_key() -> ContextEntryKey {
    ContextEntryKey::new(ENVIRONMENT_ACTIVE_CONTEXT_KEY)
}

pub fn skill_activation_context_key(skill_id: &SkillId) -> ContextEntryKey {
    ContextEntryKey::new(format!(
        "{SKILL_ACTIVATION_CONTEXT_KEY_PREFIX}{}",
        skill_id.as_str()
    ))
}

pub fn is_run_scoped_skill_activation_entry(entry: &ContextEntry) -> bool {
    matches!(entry.kind, ContextEntryKind::SkillActivation { .. })
        && entry.provider_kind.as_deref() == Some(SKILL_ACTIVATION_PROVIDER_KIND_RUN)
}

pub(crate) fn expire_run_scoped_context_entries(
    state: &mut CoreAgentState,
) -> Result<(), DomainError> {
    let before = state.context.entries.len();
    state
        .context
        .entries
        .retain(|entry| !is_run_scoped_skill_activation_entry(entry));
    if state.context.entries.len() != before {
        bump_context_revision(state)?;
    }
    Ok(())
}

fn entries_applied_proposal(
    state: &CoreAgentState,
    run_id: RunId,
    entries: Vec<ContextEntry>,
) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins {
            run_id: Some(run_id),
            ..CoreAgentJoins::default()
        },
        CoreAgentEventKind::Context(Event::EntriesApplied {
            base_revision: state.context.revision,
            entries,
        }),
    )
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::EntriesApplied {
            base_revision,
            entries,
        } => {
            validate_base_revision(state, *base_revision)?;
            apply_entries_applied(state, entries)?;
            bump_context_revision(state)?;
            Ok(())
        }
        Event::EntriesRemoved {
            base_revision,
            entry_ids,
            reason,
        } => {
            validate_base_revision(state, *base_revision)?;
            validate_removal_reason(reason)?;
            validate_entries_removable(state, entry_ids, reason)?;
            remove_context_entries(state, entry_ids)?;
            bump_context_revision(state)?;
            Ok(())
        }
        Event::KeysRemoved {
            base_revision,
            keys,
        } => {
            validate_base_revision(state, *base_revision)?;
            if keys.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "context key removal event must contain at least one key".into(),
                ));
            }
            validate_keys_removable(state, keys)?;
            for key in keys {
                remove_context_entry_by_key(state, key);
            }
            bump_context_revision(state)?;
            Ok(())
        }
        Event::KeyPrefixReplaced {
            base_revision,
            key_prefix,
            entries,
        } => {
            validate_base_revision(state, *base_revision)?;
            apply_key_prefix_replaced(state, key_prefix, entries)?;
            bump_context_revision(state)?;
            Ok(())
        }
        Event::StateReplaced {
            base_revision,
            entries,
            reason,
        } => {
            validate_base_revision(state, *base_revision)?;
            replace_context_state(state, entries, reason)?;
            bump_context_revision(state)?;
            Ok(())
        }
        Event::CompactionRequested {
            base_revision,
            trigger: _,
        } => {
            validate_base_revision(state, *base_revision)?;
            validate_compaction_requested(state)?;
            state.context.pending_compaction = true;
            bump_context_revision(state)?;
            Ok(())
        }
        Event::CompactionFinished {
            base_revision,
            status,
            failure_ref,
        } => {
            validate_base_revision(state, *base_revision)?;
            if matches!(status, ContextCompactionStatus::Succeeded) && failure_ref.is_some() {
                return Err(DomainError::InvariantViolation(
                    "successful context compaction cannot include a failure ref".to_owned(),
                ));
            }
            if !state.context.pending_compaction {
                return Err(DomainError::InvariantViolation(
                    "context compaction finished without a pending request".to_owned(),
                ));
            }
            state.context.pending_compaction = false;
            bump_context_revision(state)?;
            Ok(())
        }
    }
}

fn validate_compaction_requested(state: &CoreAgentState) -> Result<(), DomainError> {
    validate_standalone_compaction_can_start(state)?;
    if compactable_context_entry_ids(state).is_empty() {
        return Err(DomainError::InvariantViolation(
            "context compaction request must contain at least one entry".to_owned(),
        ));
    }
    Ok(())
}

fn validate_base_revision(state: &CoreAgentState, base_revision: u64) -> Result<(), DomainError> {
    if base_revision == state.context.revision {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "context event base revision {} does not match active revision {}",
            base_revision, state.context.revision
        )))
    }
}

fn bump_context_revision(state: &mut CoreAgentState) -> Result<(), DomainError> {
    state.context.revision =
        state.context.revision.checked_add(1).ok_or_else(|| {
            DomainError::InvariantViolation("context revision exhausted".to_owned())
        })?;
    Ok(())
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
    validate_no_duplicate_entry_keys(entries)?;
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

        record_entry_materialization(state, entry)?;

        if let Some(key) = entry.key.as_ref() {
            remove_context_entry_by_key(state, key);
        }

        state.context.entries.push(entry.clone());
        state.id_cursors.last_context_item_id = entry.entry_id.as_u64();
    }
    Ok(())
}

fn validate_no_duplicate_entry_keys(entries: &[ContextEntry]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        if let Some(key) = entry.key.as_ref() {
            if !seen.insert(key.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "duplicate context key {} in entries event",
                    key
                )));
            }
        }
    }
    Ok(())
}

fn apply_key_prefix_replaced(
    state: &mut CoreAgentState,
    key_prefix: &ContextEntryKey,
    entries: &[ContextEntry],
) -> Result<(), DomainError> {
    validate_key_prefix_replacement_entries(state, key_prefix, entries)?;
    validate_prefix_entries_removable(state, key_prefix)?;
    remove_context_entries_by_key_prefix(state, key_prefix);
    if !entries.is_empty() {
        apply_entries_applied(state, entries)?;
    }
    Ok(())
}

fn validate_key_prefix_replacement_entries(
    state: &CoreAgentState,
    key_prefix: &ContextEntryKey,
    entries: &[ContextEntry],
) -> Result<(), DomainError> {
    if entries.is_empty() && !has_active_key_with_prefix(state, key_prefix) {
        return Err(DomainError::InvariantViolation(format!(
            "context key prefix replacement {} has no active entries and no replacement entries",
            key_prefix
        )));
    }
    validate_no_duplicate_entry_keys(entries)?;
    for entry in entries {
        let Some(key) = entry.key.as_ref() else {
            return Err(DomainError::InvariantViolation(format!(
                "context key prefix replacement entry {} must have a key",
                entry.entry_id
            )));
        };
        if !context_key_starts_with(key, key_prefix) {
            return Err(DomainError::InvariantViolation(format!(
                "context key prefix replacement entry {} has key {} outside prefix {}",
                entry.entry_id, key, key_prefix
            )));
        }
        if !matches!(entry.source, ContextEntrySource::ContextEdit) {
            return Err(DomainError::InvariantViolation(format!(
                "context key prefix replacement entry {} must use context edit source",
                entry.entry_id
            )));
        }
        let input = context_entry_input_from_active(entry);
        validate_external_context_edit_entry(key, &input)?;
    }
    Ok(())
}

fn record_entry_materialization(
    state: &mut CoreAgentState,
    entry: &ContextEntry,
) -> Result<(), DomainError> {
    match &entry.source {
        ContextEntrySource::RunInput {
            run_id,
            input_index,
        } => {
            let active_run = crate::core::components::run::active_run_mut(state, *run_id)?;
            let index = *input_index as usize;
            let RunSource::Input { input } = &active_run.source else {
                return Err(DomainError::InvariantViolation(format!(
                    "run input context entry {} references context-triggered run {}",
                    entry.entry_id, run_id
                )));
            };
            let Some(expected) = input.get(index) else {
                return Err(DomainError::InvariantViolation(format!(
                    "run input context entry {} references missing input index {}",
                    entry.entry_id, input_index
                )));
            };
            validate_entry_matches_input(entry, expected, true)?;
            if active_run.input_entry_ids.len() != index {
                return Err(DomainError::InvariantViolation(format!(
                    "run input context entry {} expected input index {}, got {}",
                    entry.entry_id,
                    active_run.input_entry_ids.len(),
                    input_index
                )));
            }
            active_run.input_entry_ids.push(entry.entry_id);
            Ok(())
        }
        ContextEntrySource::Steering {
            run_id,
            steering_id,
            input_index,
        } => {
            let active_run = crate::core::components::run::active_run_mut(state, *run_id)?;
            let Some(steering) = active_run
                .steering
                .iter_mut()
                .find(|steering| steering.steering_id == *steering_id)
            else {
                return Err(DomainError::InvariantViolation(format!(
                    "steering context entry {} references missing steering batch {}",
                    entry.entry_id, steering_id
                )));
            };
            let index = *input_index as usize;
            let Some(expected) = steering.input.get(index) else {
                return Err(DomainError::InvariantViolation(format!(
                    "steering context entry {} references missing input index {}",
                    entry.entry_id, input_index
                )));
            };
            validate_entry_matches_input(entry, expected, false)?;
            if steering.entry_ids.len() != index {
                return Err(DomainError::InvariantViolation(format!(
                    "steering context entry {} expected input index {}, got {}",
                    entry.entry_id,
                    steering.entry_ids.len(),
                    input_index
                )));
            }
            steering.entry_ids.push(entry.entry_id);
            Ok(())
        }
        ContextEntrySource::ContextEdit
        | ContextEntrySource::AssistantOutput { .. }
        | ContextEntrySource::Tool { .. }
        | ContextEntrySource::Reasoning { .. }
        | ContextEntrySource::Runtime { .. } => Ok(()),
    }
}

fn validate_entry_matches_input(
    entry: &ContextEntry,
    input: &ContextEntryInput,
    allow_key: bool,
) -> Result<(), DomainError> {
    if entry.key.is_some() && !allow_key {
        return Err(DomainError::InvariantViolation(format!(
            "run materialized context entry {} must not have a key",
            entry.entry_id
        )));
    }
    if entry.kind != input.kind
        || entry.content_ref != input.content_ref
        || entry.media_type != input.media_type
        || entry.preview != input.preview
        || entry.provider_kind != input.provider_kind
        || entry.provider_item_id != input.provider_item_id
        || entry.token_estimate != input.token_estimate
    {
        return Err(DomainError::InvariantViolation(format!(
            "context entry {} does not match accepted input payload",
            entry.entry_id
        )));
    }
    Ok(())
}

fn validate_removal_reason(reason: &ContextRemovalReason) -> Result<(), DomainError> {
    match reason {
        ContextRemovalReason::Pruned | ContextRemovalReason::ProviderCompacted => Ok(()),
    }
}

fn validate_entries_removable(
    state: &CoreAgentState,
    entry_ids: &[ContextEntryId],
    _reason: &ContextRemovalReason,
) -> Result<(), DomainError> {
    for entry_id in entry_ids {
        validate_entry_is_not_unconsumed_active_run_input(state, *entry_id)?;
    }
    Ok(())
}

fn validate_keys_removable(
    state: &CoreAgentState,
    keys: &[ContextEntryKey],
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for key in keys {
        if !seen.insert(key.clone()) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate context key removal {}",
                key
            )));
        }
        validate_context_key_exists(state, key)?;
    }
    Ok(())
}

fn validate_prefix_entries_removable(
    state: &CoreAgentState,
    key_prefix: &ContextEntryKey,
) -> Result<(), DomainError> {
    for entry in &state.context.entries {
        if entry
            .key
            .as_ref()
            .is_some_and(|key| context_key_starts_with(key, key_prefix))
        {
            validate_entry_is_not_unconsumed_active_run_input(state, entry.entry_id)?;
        }
    }
    Ok(())
}

fn validate_entry_is_not_unconsumed_active_run_input(
    state: &CoreAgentState,
    entry_id: ContextEntryId,
) -> Result<(), DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(());
    };

    if active_run.input_consumed_by_turn_id.is_none()
        && active_run.input_entry_ids.contains(&entry_id)
    {
        return Err(DomainError::InvariantViolation(format!(
            "cannot remove unconsumed run input context entry {}",
            entry_id
        )));
    }

    for steering in &active_run.steering {
        if steering.consumed_by_turn_id.is_none() && steering.entry_ids.contains(&entry_id) {
            return Err(DomainError::InvariantViolation(format!(
                "cannot remove unconsumed steering context entry {}",
                entry_id
            )));
        }
    }

    Ok(())
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

fn remove_context_entries_by_key_prefix(state: &mut CoreAgentState, key_prefix: &ContextEntryKey) {
    state.context.entries.retain(|entry| {
        !entry
            .key
            .as_ref()
            .is_some_and(|key| context_key_starts_with(key, key_prefix))
    });
}

fn has_active_key_with_prefix(state: &CoreAgentState, key_prefix: &ContextEntryKey) -> bool {
    state.context.entries.iter().any(|entry| {
        entry
            .key
            .as_ref()
            .is_some_and(|key| context_key_starts_with(key, key_prefix))
    })
}

fn context_key_starts_with(key: &ContextEntryKey, key_prefix: &ContextEntryKey) -> bool {
    key.as_str() == key_prefix.as_str()
        || key
            .as_str()
            .strip_prefix(key_prefix.as_str())
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn context_entry_input_from_active(entry: &ContextEntry) -> ContextEntryInput {
    ContextEntryInput {
        kind: entry.kind.clone(),
        content_ref: entry.content_ref.clone(),
        media_type: entry.media_type.clone(),
        preview: entry.preview.clone(),
        provider_kind: entry.provider_kind.clone(),
        provider_item_id: entry.provider_item_id.clone(),
        token_estimate: entry.token_estimate.clone(),
    }
}

fn replace_context_state(
    state: &mut CoreAgentState,
    entries: &[ContextEntry],
    reason: &ContextRewriteReason,
) -> Result<(), DomainError> {
    validate_rewrite_reason(state, reason)?;
    validate_replacement_entries(state, entries)?;
    validate_rewrite_preserves_unconsumed_entries(state, entries)?;

    if let Some(last) = entries.last() {
        state.id_cursors.last_context_item_id = last
            .entry_id
            .as_u64()
            .max(state.id_cursors.last_context_item_id);
    }
    state.context.entries = entries.to_vec();
    Ok(())
}

fn validate_rewrite_preserves_unconsumed_entries(
    state: &CoreAgentState,
    replacement_entries: &[ContextEntry],
) -> Result<(), DomainError> {
    let replacement_ids = replacement_entries
        .iter()
        .map(|entry| entry.entry_id)
        .collect::<BTreeSet<_>>();
    for entry in &state.context.entries {
        if !replacement_ids.contains(&entry.entry_id) {
            validate_entry_is_not_unconsumed_active_run_input(state, entry.entry_id)?;
        }
    }
    Ok(())
}

fn validate_rewrite_reason(
    _state: &CoreAgentState,
    reason: &ContextRewriteReason,
) -> Result<(), DomainError> {
    match reason {
        ContextRewriteReason::Pruned
        | ContextRewriteReason::PolicyChanged
        | ContextRewriteReason::ProviderCompacted => Ok(()),
    }
}

fn validate_replacement_entries(
    state: &CoreAgentState,
    entries: &[ContextEntry],
) -> Result<(), DomainError> {
    let mut seen_ids = BTreeSet::new();
    let mut seen_keys = BTreeSet::new();
    let mut previous_entry_id = None;

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
                return Err(DomainError::InvariantViolation(format!(
                    "replacement context entry {} is not an active entry",
                    entry.entry_id
                )));
            }
        }
    }

    Ok(())
}
