use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    BlobRef, CompactionPolicy, ContextEntryKey, ContextItemId, CoreAgentEvent,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentState, CoreAgentStatus, DomainError,
    PlanningError, ProviderApiKind, RunId, RunSource, RunStatus, SteeringId, ToolBatchId,
    ToolCallId, ToolName, TurnId,
};

const RESERVED_RUN_CONTEXT_KEY_PREFIX: &str = "run";
const INSTRUCTIONS_KEY_PREFIX: &str = "instructions.";
/// Superseded catalog versions kept per key before the oldest is removed.
/// A superseded catalog stays rendered so the provider prefix cache holds;
/// the cap bounds how many stale versions a churning catalog can accumulate
/// between prefix rewrites (one invalidation per `CAP` changes, not per
/// change).
pub const SUPERSEDED_CATALOG_CAP: usize = 5;
pub const OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND: &str = "openai.responses.compaction";
pub const OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND: &str = "openai.completions.compaction";
pub const ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND: &str = "anthropic.messages.compaction";

pub type ContextEntryId = ContextItemId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// Applies new immutable entries to active context. Unkeyed entries append;
    /// keyed entries replace the previous active entry for that key — except
    /// catalog kinds, which *supersede* it: the previous version stays active
    /// (and rendered byte-for-byte, so the provider prefix cache holds), the
    /// new entry records `supersedes`, and versions beyond
    /// `SUPERSEDED_CATALOG_CAP` are dropped oldest-first.
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
    /// Replaces active entries in place, by id. Each replacement keeps the
    /// entry's id, position, key, source, and kind (so tool-call pairing
    /// holds); the replaced content stays in the event log.
    EntriesReplaced {
        base_revision: u64,
        entries: Vec<ContextEntry>,
    },
    CompactionRequested {
        base_revision: u64,
        trigger: ContextCompactionTrigger,
        plan: ContextCompactionPlan,
    },
    CompactionQueued {
        base_revision: u64,
    },
    CompactionFinished {
        base_revision: u64,
        status: ContextCompactionStatus,
        failure_ref: Option<BlobRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<crate::LlmUsage>,
        #[serde(default)]
        calls: u32,
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
    #[serde(default)]
    pub compaction: ContextCompactionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_generation: Option<ContextGenerationMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_observation: Option<ContextUsageObservation>,
}

impl ContextState {
    pub fn last_generation_model(&self) -> Option<&crate::ModelSelection> {
        self.last_generation
            .as_ref()
            .map(|generation| &generation.model)
    }

    /// An observation is usable only for the context revision it measured.
    pub fn observed_tokens(&self) -> Option<u32> {
        self.usage_observation
            .as_ref()
            .filter(|observation| observation.context_revision == self.revision)
            .map(|observation| observation.tokens)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionState {
    pub phase: ContextCompactionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_finished_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_manual_finished_revision: Option<u64>,
    /// Where the last failed proactive attempt left off, until one succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_floor: Option<CompactionFailureFloor>,
}

/// Proactive compaction that failed is not tried again until the context has grown
/// past this by about a tenth. Without it, a compaction that keeps failing is paid
/// for at every boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionFailureFloor {
    /// The token estimate the failed attempt fired on, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    /// Active entries when the attempt failed.
    pub entries: usize,
}

fn grown_past_floor(value: u64, floor: u64) -> bool {
    value >= floor.saturating_add(floor / 10)
}

impl ContextCompactionState {
    pub fn is_pending(&self) -> bool {
        matches!(self.phase, ContextCompactionPhase::Pending(_))
    }

    pub fn is_queued(&self) -> bool {
        matches!(self.phase, ContextCompactionPhase::QueuedManual)
    }

    pub fn pending_plan(&self) -> Option<&ContextCompactionPlan> {
        match &self.phase {
            ContextCompactionPhase::Pending(plan) => Some(plan),
            ContextCompactionPhase::Idle | ContextCompactionPhase::QueuedManual => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionPhase {
    #[default]
    Idle,
    QueuedManual,
    Pending(ContextCompactionPlan),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextGenerationMetadata {
    pub model: crate::ModelSelection,
    pub input_limit_tokens: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsageObservation {
    pub context_revision: u64,
    pub tokens: u32,
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
    ProviderCompacted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionTrigger {
    Manual,
    HighWatermark,
    ContextLimit,
}

/// A frozen covered prefix. Tail entries keep their identities and native bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub covered_entry_ids: Vec<ContextEntryId>,
    pub trigger: ContextCompactionTrigger,
    /// The token estimate a proactive request fired on. A failure keeps it as its
    /// floor, because the request itself moves the revision the estimate depends on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<u32>,
}

/// Active entries at which compaction runs whatever the tokens say. Every generation's
/// activity input and reduced state carry all entry metadata, so entries have a cost of
/// their own that token compaction does not control: a tool-heavy session on a large
/// window could otherwise reach the bootstrap budget long before its threshold.
pub const ENTRY_COUNT_COMPACTION_TRIGGER: usize = 1_500;
/// Active entries past which a run does not go on with no compaction tried. Reduced
/// state crosses the bootstrap budget near 3,000 entries, and a session over that
/// budget can no longer load. This is reached only with compaction disabled or failing:
/// a run that cannot bring the count down fails while the session can still be saved.
pub const ENTRY_GUARD_LIMIT: usize = 2_000;

pub const STANDALONE_COMPACTION_SOURCE: &str = "standalone_compaction_prefix";
pub const MAX_CONTEXT_RECOVERY_ATTEMPTS: u32 = 2;

pub(crate) fn is_standalone_prefix(entry: &ContextEntry) -> bool {
    matches!(&entry.source, ContextEntrySource::Runtime { label } if label == STANDALONE_COMPACTION_SOURCE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompactionStatus {
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Application-supplied display provenance; independent of role and insertion source.
    pub origin: Option<String>,
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
    /// Immutable payload reference and encoding, also used for run outputs.
    pub content: crate::ContentRef,
    /// Short display text for projections and logs; not authoritative model
    /// input. One exception: media entries carry the `[kind: name]` form from
    /// `media_preview` and gateway admission. Its name reaches the model, and
    /// its `[document` prefix marks a text document on a user message with no
    /// provider kind, so adapters read it when lowering.
    pub preview: Option<String>,
    /// Immutable artifact recording this entry's origin or construction.
    pub provenance_ref: Option<BlobRef>,
    /// Optional accounting estimate used by context planning.
    pub token_estimate: Option<TokenEstimate>,
    /// The earlier version of this keyed catalog that this entry replaces
    /// as the current one. The earlier entry stays active until a prefix
    /// rewrite or the per-key cap removes it; renderers mark this entry as
    /// the update. Only catalog kinds supersede; other keyed entries replace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<ContextEntryId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntryInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Application-supplied display provenance; independent of role and insertion source.
    pub origin: Option<String>,
    pub kind: ContextEntryKind,
    pub content: crate::ContentRef,
    pub preview: Option<String>,
    /// Immutable artifact recording this entry's origin or construction.
    pub provenance_ref: Option<BlobRef>,
    pub token_estimate: Option<TokenEstimate>,
}

impl ContextEntryInput {
    fn commit(
        self,
        entry_id: ContextEntryId,
        key: Option<ContextEntryKey>,
        source: ContextEntrySource,
        supersedes: Option<ContextEntryId>,
    ) -> ContextEntry {
        ContextEntry {
            entry_id,
            key,
            kind: self.kind,
            source,
            content: self.content,
            preview: self.preview,
            origin: self.origin,
            provenance_ref: self.provenance_ref,
            token_estimate: self.token_estimate,
            supersedes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEntryKind {
    Message {
        role: ContextMessageRole,
    },
    Instructions,
    /// A catalog: an opaque text document under a stable key
    /// that tells the model what it may pick from (a directory, a roster, a
    /// menu). Runtime publishers and clients use the same representation.
    /// A changed catalog supersedes its previous version.
    Catalog {
        title: String,
    },
    ToolCall {
        call_id: ToolCallId,
        name: ToolName,
    },
    ToolResult {
        call_id: ToolCallId,
        is_error: bool,
    },
    ReasoningState,
    ProviderOpaque,
    McpApprovalResponse {
        approval_request_id: String,
        approve: bool,
    },
}

/// Catalog kinds supersede on keyed replacement instead of removing the
/// previous version: menus change rarely relative to turns, and rewriting
/// them mid-context would invalidate the provider prefix cache from that
/// position for every session that outlives a catalog edit.
pub fn is_supersedable_catalog_kind(kind: &ContextEntryKind) -> bool {
    matches!(kind, ContextEntryKind::Catalog { .. })
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
    ApprovalDecision {
        run_id: RunId,
        approval_id: crate::ApprovalId,
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

    if state.context.entries.iter().any(is_standalone_prefix) {
        for entry in state
            .context
            .entries
            .iter()
            .filter(|entry| is_standalone_prefix(entry))
        {
            if seen.insert(entry.entry_id) {
                entry_ids.push(entry.entry_id);
            }
        }
    }

    // Everything else, catalogs included, renders at its entry position.
    // Catalogs are first published before the first run, so a fresh session
    // still sees them right after the instructions; a refreshed catalog
    // lands at the tail and supersedes the earlier version, which stays in
    // place so the rendered prefix does not move.
    entry_ids.extend(tool_groups_adjacent(
        state
            .context
            .entries
            .iter()
            .filter(|entry| !seen.contains(&entry.entry_id)),
    ));

    entry_ids
}

/// Arrival order, except that nothing separates a tool call from its result:
/// providers want the answer right behind the call, and the log records what
/// really happened. A client or bot can append while a batch runs, and a media
/// entry can follow one result of several. Such an entry renders behind the
/// group's last result. The calling turn's own output stays where it is, since
/// a provider may need its reasoning between the calls.
///
/// Every request renders through here, so the order holds wherever it reaches a
/// provider, and it depends on the entries alone, so it does not move once sent.
/// A later turn's output closes a group that was never answered, which keeps an
/// orphaned call from holding back the rest of the session.
fn tool_groups_adjacent<'a>(
    entries: impl Iterator<Item = &'a ContextEntry>,
) -> Vec<ContextEntryId> {
    let mut rendered = Vec::new();
    let mut held = Vec::new();
    let mut unanswered = BTreeSet::new();
    let mut calling_turn = None;
    for entry in entries {
        let output_turn = match entry.source {
            ContextEntrySource::AssistantOutput { run_id, turn_id }
            | ContextEntrySource::Reasoning { run_id, turn_id } => Some((run_id, turn_id)),
            _ => None,
        };
        if output_turn.is_some() && output_turn != calling_turn {
            rendered.append(&mut held);
            unanswered.clear();
            calling_turn = output_turn;
        }
        let in_group = match &entry.kind {
            ContextEntryKind::ToolCall { call_id, .. } => {
                unanswered.insert(call_id);
                true
            }
            ContextEntryKind::ToolResult { call_id, .. } => unanswered.remove(call_id),
            _ => output_turn.is_some(),
        };
        if in_group || unanswered.is_empty() {
            rendered.push(entry.entry_id);
        } else {
            held.push(entry.entry_id);
        }
        if unanswered.is_empty() {
            rendered.append(&mut held);
        }
    }
    rendered.append(&mut held);
    rendered
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
            entry_by_id(state, *entry_id).is_some_and(|entry| is_compactable_entry(state, entry))
        })
        .collect()
}

pub(crate) fn standalone_prefix_ids(state: &CoreAgentState) -> Vec<ContextEntryId> {
    let ids = compactable_context_entry_ids(state);
    let mut boundaries = Vec::new();
    let mut last = None;
    for (index, id) in ids.iter().enumerate() {
        let entry = entry_by_id(state, *id).expect("planned entry");
        if let ContextEntrySource::AssistantOutput { run_id, turn_id }
        | ContextEntrySource::Reasoning { run_id, turn_id } = &entry.source
        {
            let turn = (*run_id, *turn_id);
            if last != Some(turn) {
                // Keep the user input immediately preceding a retained turn too.
                let mut boundary = index;
                while boundary > 0 {
                    let previous = entry_by_id(state, ids[boundary - 1]).expect("planned entry");
                    if matches!(previous.source,
                        ContextEntrySource::RunInput { run_id: previous, .. }
                        | ContextEntrySource::Steering { run_id: previous, .. } if previous == *run_id)
                    {
                        boundary -= 1;
                    } else {
                        break;
                    }
                }
                boundaries.push(boundary);
                last = Some(turn);
            }
        }
    }
    // If the first recovery still leaves an overflowing tail, summarize the
    // complete settled window on the second attempt. Tool groups stay atomic.
    let recovery = state.runs.active.as_ref().map(|run| &run.context_recovery);
    let retry_overflow = recovery.is_some_and(|recovery| recovery.attempts > 0)
        && state
            .runs
            .active
            .as_ref()
            .and_then(|run| run.turns.values().next_back())
            .is_some_and(|turn| {
                recovery.and_then(|recovery| recovery.recovered_turn_id) != Some(turn.turn_id)
                    && matches!(turn.outcome, Some(crate::TurnOutcome::ContextLimit { .. }))
            });
    // At the entry guard the tail may be what holds the count up, and a run stopped
    // there starts no turn that would roll it out. Only the whole window can clear it.
    let at_entry_guard = state.context.entries.len() >= ENTRY_GUARD_LIMIT;
    let cut = if retry_overflow || at_entry_guard {
        ids.len()
    } else {
        boundaries
            .iter()
            .rev()
            .nth(1)
            .copied()
            .or_else(|| boundaries.first().copied())
            .unwrap_or(ids.len())
    };
    let cut = if cut == 0 { ids.len() } else { cut };
    ids.into_iter()
        .take(cut)
        .take_while(|id| validate_entry_is_not_unconsumed_active_run_input(state, *id).is_ok())
        .collect()
}

/// Capacity of the effective generation/compaction model, with no provider I/O. A
/// run reads what was resolved for it, then the limit the document declares for its
/// own model. An idle session has no run to ask, so manual compaction reads what the
/// last generation recorded for its model.
pub fn compaction_input_limit_tokens(state: &CoreAgentState) -> Option<u32> {
    let config = state.lifecycle.config.as_ref()?;
    let run = state.runs.active.as_ref();
    let model = run
        .map(|run| {
            run.run_config
                .model_override
                .as_ref()
                .unwrap_or(&config.model)
        })
        .or(state.context.last_generation_model())
        .unwrap_or(&config.model);
    run.and_then(|run| run.input_limit_tokens)
        .or_else(|| {
            (model == &config.model)
                .then_some(config.context.input_limit_tokens)
                .flatten()
        })
        .or_else(|| {
            run.is_none()
                .then(|| state.context.last_generation.as_ref())
                .flatten()
                .and_then(|generation| generation.input_limit_tokens)
        })
}

pub(crate) fn compaction_safe_boundary(state: &CoreAgentState) -> bool {
    !state.context.compaction.is_pending()
        && !has_active_nonterminal_tool_batch(state)
        && state.runs.active.as_ref().is_none_or(|run| {
            run.status == RunStatus::Active
                && run.active_turn_id.is_none()
                && run.active_tool_batch_id.is_none()
                && !crate::core::components::run::has_unstarted_tool_call_turn(run)
        })
}

/// Configuration entries (instructions and current catalogs)
/// survive compaction; conversation does not. A superseded catalog version
/// is stale configuration kept only for prefix stability, so it is the
/// first thing a prefix rewrite may drop.
fn is_compactable_entry(state: &CoreAgentState, entry: &ContextEntry) -> bool {
    match &entry.kind {
        ContextEntryKind::Instructions => false,
        kind if is_supersedable_catalog_kind(kind) => {
            is_superseded_context_entry(state, entry.entry_id)
        }
        _ => true,
    }
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

    // A batch admitted after the turn started has no entries yet, and an empty
    // containment test would mark it consumed before any request carried it.
    for steering in &mut active_run.steering {
        if steering.consumed_by_turn_id.is_none()
            && steering.entry_ids.len() == steering.input.len()
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

pub(crate) fn combined_token_estimate<'a>(
    entries: impl IntoIterator<Item = &'a ContextEntry>,
) -> Option<TokenEstimate> {
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
    context_entries_after(state, state.id_cursors.last_context_item_id, inputs)
}

/// As [`context_entries_from_inputs`], numbering after entries proposed earlier in
/// the same append.
fn context_entries_after(
    state: &CoreAgentState,
    last_entry_id: u64,
    inputs: Vec<(
        Option<ContextEntryKey>,
        ContextEntrySource,
        ContextEntryInput,
    )>,
) -> Result<Vec<ContextEntry>, DomainError> {
    let mut next_entry_id = last_entry_id;
    inputs
        .into_iter()
        .map(|(key, source, entry)| {
            next_entry_id = next_entry_id.checked_add(1).ok_or_else(|| {
                DomainError::InvariantViolation("context entry id cursor exhausted".to_owned())
            })?;
            let supersedes = key
                .as_ref()
                .and_then(|key| supersede_target(state, key, &entry.kind));
            Ok(entry.commit(ContextEntryId::new(next_entry_id), key, source, supersedes))
        })
        .collect()
}

/// The active entry a keyed catalog write supersedes: the key's current
/// entry, when both it and the new entry are catalog kinds. Any other keyed
/// write replaces the current entry outright.
fn supersede_target(
    state: &CoreAgentState,
    key: &ContextEntryKey,
    kind: &ContextEntryKind,
) -> Option<ContextEntryId> {
    if !is_supersedable_catalog_kind(kind) {
        return None;
    }
    current_key_entry(state, key)
        .filter(|current| is_supersedable_catalog_kind(&current.kind))
        .map(|current| current.entry_id)
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

    match &entry.kind {
        ContextEntryKind::ProviderOpaque => Ok(()),
        ContextEntryKind::McpApprovalResponse { .. } => Err(DomainError::InvariantViolation(
            "MCP approval responses are runtime-owned context".to_owned(),
        )),
        ContextEntryKind::Message {
            role: ContextMessageRole::User,
        } => Ok(()),
        ContextEntryKind::Catalog { title } if title.trim().is_empty() => Err(
            DomainError::InvariantViolation(format!("catalog context entry {} needs a title", key)),
        ),
        ContextEntryKind::Catalog { .. } => Ok(()),
        ContextEntryKind::Instructions => Err(DomainError::InvariantViolation(format!(
            "instruction context entry requires an {}* key, got {}",
            INSTRUCTIONS_KEY_PREFIX, key
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

    if state.context.compaction.is_pending()
        && state
            .context
            .compaction
            .pending_plan()
            .and_then(|plan| plan.run_id)
            .is_some_and(|run_id| {
                state
                    .runs
                    .active
                    .as_ref()
                    .is_none_or(|run| run.run_id != run_id || run.status != RunStatus::Active)
            })
    {
        return Ok(vec![compaction_abandoned_proposal(state)]);
    }
    // A pending compaction is frozen at its revision. A run that has just started has
    // input waiting, and writing it now would orphan the request.
    if state.context.compaction.is_pending() {
        return Ok(Vec::new());
    }
    if let Some(proposal) = provider_compacted_prune_proposal(state)? {
        return Ok(vec![proposal]);
    }

    if let Some(proposal) = next_compaction_proposal(state) {
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

    // Steering materializes between turns only: an in-flight turn's request is
    // frozen at its planned context revision and the hosted runtime re-derives
    // it from state, so context must not move under it. An open batch owes its
    // results first, so a steer lands behind them in the log as well as in the
    // request. Tooling plans ahead of this, so a finished tool-call turn already
    // has its batch here.
    if active_run.active_turn_id.is_some() || active_run.active_tool_batch_id.is_some() {
        return Ok(Vec::new());
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
) -> CoreAgentEventProposal {
    if !compaction_safe_boundary(state) {
        return CoreAgentEventProposal::new(
            CoreAgentJoins::default(),
            CoreAgentEvent::Context(Event::CompactionQueued {
                base_revision: state.context.revision,
            }),
        );
    }
    compaction_requested_proposal(state, ContextCompactionTrigger::Manual, None)
}

/// Whether the planner may compact on its own. A manual request needs no policy.
pub(crate) fn compaction_enabled(config: &crate::SessionConfig) -> bool {
    !matches!(
        config.context.compaction,
        None | Some(CompactionPolicy::Disabled)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryGuard {
    Clear,
    /// One attempt for this run, whatever the failure floor says: the floor must not
    /// hold off the only cure, and each new run trying afresh is what lets a temporary
    /// outage heal with nobody at the controls.
    Compact,
    FailRun,
}

/// What this turn boundary owes [`ENTRY_GUARD_LIMIT`].
pub(crate) fn entry_guard(state: &CoreAgentState, run: &crate::ActiveRun) -> EntryGuard {
    if state.context.entries.len() < ENTRY_GUARD_LIMIT || !compaction_safe_boundary(state) {
        return EntryGuard::Clear;
    }
    if !run.entry_guard_attempted
        && state
            .lifecycle
            .config
            .as_ref()
            .is_some_and(compaction_enabled)
        && !standalone_prefix_ids(state).is_empty()
    {
        return EntryGuard::Compact;
    }
    EntryGuard::FailRun
}

/// The compaction this boundary calls for: a queued manual request, overflow
/// recovery, or the high watermark. Only the last is subject to the failure floor.
fn next_compaction_proposal(state: &CoreAgentState) -> Option<CoreAgentEventProposal> {
    if !compaction_safe_boundary(state) {
        return None;
    }
    if state.context.compaction.is_queued() {
        // Edits landing after the request was queued can leave nothing to cover.
        // The caller is still waiting, so it gets an answer.
        if standalone_prefix_ids(state).is_empty() {
            return Some(compaction_abandoned_proposal(state));
        }
        return Some(compaction_requested_proposal(
            state,
            ContextCompactionTrigger::Manual,
            None,
        ));
    }
    let config = state.lifecycle.config.as_ref()?;
    let recovery = state.runs.active.as_ref().map(|run| &run.context_recovery);
    let latest = state
        .runs
        .active
        .as_ref()
        .and_then(|run| run.turns.values().next_back());
    // A failed or provider-cancelled turn only waits for its steering to be written
    // down before the run ends. It needs no room, and with the provider down a
    // compaction would hold that failure back for its whole retry budget. A run out
    // of turns is the same: nothing will use the room.
    if latest.is_some_and(|turn| {
        matches!(
            turn.status,
            crate::TurnStatus::Failed | crate::TurnStatus::Cancelled
        )
    }) || state
        .runs
        .active
        .as_ref()
        .is_some_and(|run| crate::core::components::run::turn_limit_reached(state, run))
    {
        return None;
    }
    let overflow = latest.is_some_and(|turn| {
        matches!(turn.outcome, Some(crate::TurnOutcome::ContextLimit { .. }))
            && recovery.and_then(|recovery| recovery.recovered_turn_id) != Some(turn.turn_id)
    });
    if overflow {
        if compaction_enabled(config)
            && recovery.is_none_or(|recovery| recovery.attempts < MAX_CONTEXT_RECOVERY_ATTEMPTS)
            && !standalone_prefix_ids(state).is_empty()
        {
            return Some(compaction_requested_proposal(
                state,
                ContextCompactionTrigger::ContextLimit,
                None,
            ));
        }
        return None;
    }
    high_watermark_compaction_proposal(state, config)
}

/// Proactive compaction: enough context that it is worth making room before the
/// provider has to refuse a request.
fn high_watermark_compaction_proposal(
    state: &CoreAgentState,
    config: &crate::SessionConfig,
) -> Option<CoreAgentEventProposal> {
    if !compaction_enabled(config) {
        return None;
    }
    // Room is worth paying for only when a run will use it, so a run ending over the
    // watermark compacts when the next one starts, before its input arrives. Run
    // terminals flush after the drive settles, so compacting here would hold a
    // sub-agent's parent or a bot lane behind a summary nobody may read. A queued run
    // starts before this planner looks, so an active run is the only case.
    let run = state.runs.active.as_ref()?;
    match entry_guard(state, run) {
        EntryGuard::Compact => {
            return Some(compaction_requested_proposal(
                state,
                ContextCompactionTrigger::HighWatermark,
                high_watermark_token_estimate(state),
            ));
        }
        EntryGuard::FailRun => return None,
        EntryGuard::Clear => {}
    }
    // Nothing has moved since the last attempt, so another would see the same context.
    if state.context.compaction.last_finished_revision == Some(state.context.revision) {
        return None;
    }
    // Every enabled policy gets this: a provider's own compaction bounds tokens only.
    if entry_count_calls_for_compaction(state) {
        return Some(compaction_requested_proposal(
            state,
            ContextCompactionTrigger::HighWatermark,
            high_watermark_token_estimate(state),
        ));
    }
    if !matches!(
        config.context.compaction,
        Some(CompactionPolicy::ProviderStandalone { .. })
    ) && !state.context.entries.iter().any(|entry| {
        entry.content.provider_kind.as_deref() == Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND)
            && matches!(entry.kind, ContextEntryKind::ProviderOpaque)
            && is_standalone_prefix(entry)
    }) {
        return None;
    }
    let threshold = match config.context.compaction {
        Some(
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens,
                ..
            }
            | CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens,
            },
        ) => compact_threshold_tokens,
        _ => None,
    }
    .or_else(|| compaction_input_limit_tokens(state).map(|limit| limit.saturating_mul(4) / 5))?;
    let tokens = high_watermark_token_estimate(state)?;
    let floor = state.context.compaction.failure_floor.as_ref();
    if tokens < threshold
        || floor
            .and_then(|floor| floor.tokens)
            .is_some_and(|floor| !grown_past_floor(tokens.into(), floor.into()))
        || standalone_prefix_ids(state).is_empty()
    {
        return None;
    }
    Some(compaction_requested_proposal(
        state,
        ContextCompactionTrigger::HighWatermark,
        Some(tokens),
    ))
}

/// The count is at the trigger, past the floor of any failed attempt, and a summary in
/// place of the prefix would get it back under. Without the last check, a retained tail
/// that large would re-summarize the summary at every boundary.
fn entry_count_calls_for_compaction(state: &CoreAgentState) -> bool {
    let entries = state.context.entries.len();
    entries >= ENTRY_COUNT_COMPACTION_TRIGGER
        && state
            .context
            .compaction
            .failure_floor
            .as_ref()
            .is_none_or(|floor| grown_past_floor(entries as u64, floor.entries as u64))
        && entries - standalone_prefix_ids(state).len() + 1 < ENTRY_COUNT_COMPACTION_TRIGGER
}

/// The best token count there is for the context as it stands.
fn high_watermark_token_estimate(state: &CoreAgentState) -> Option<u32> {
    combined_token_estimate(&state.context.entries)
        .or_else(|| {
            combined_token_estimate(
                state
                    .context
                    .entries
                    .iter()
                    .filter(|entry| is_compactable_entry(state, entry)),
            )
        })
        .map(|estimate| estimate.tokens)
        .or_else(|| state.context.observed_tokens())
        .or_else(|| {
            // Tool results move the revision, so between turns the last generation's
            // count is the only one there is. A compaction that finished after that
            // turn was planned has made it out of date.
            let turn = state.runs.active.as_ref()?.turns.values().next_back()?;
            let finished = state.context.compaction.last_finished_revision;
            if turn.planned_request.as_ref().is_some_and(|planned| {
                finished.is_some_and(|finished| finished > planned.context_revision)
            }) {
                return None;
            }
            let facts = turn.facts.as_ref()?;
            facts.context_token_estimate.as_ref().map(|estimate| {
                estimate.tokens.saturating_add(
                    facts
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.output_tokens)
                        .unwrap_or(0),
                )
            })
        })
}

/// Ends a pending or queued compaction that no longer has anything to do.
pub(crate) fn compaction_abandoned_proposal(state: &CoreAgentState) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEvent::Context(Event::CompactionFinished {
            base_revision: state.context.revision,
            status: ContextCompactionStatus::Failed,
            failure_ref: None,
            usage: None,
            calls: 0,
        }),
    )
}

fn compaction_requested_proposal(
    state: &CoreAgentState,
    trigger: ContextCompactionTrigger,
    estimated_tokens: Option<u32>,
) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEvent::Context(Event::CompactionRequested {
            base_revision: state.context.revision,
            trigger,
            plan: ContextCompactionPlan {
                run_id: state.runs.active.as_ref().map(|run| run.run_id),
                covered_entry_ids: standalone_prefix_ids(state),
                trigger,
                estimated_tokens,
            },
        }),
    )
}

fn missing_run_input_entries(state: &CoreAgentState) -> Result<Vec<ContextEntry>, DomainError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    let RunSource::Input { input } = &active_run.source;
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

pub(crate) fn missing_steering_entries(
    state: &CoreAgentState,
) -> Result<Vec<ContextEntry>, DomainError> {
    missing_steering_entries_after(state, state.id_cursors.last_context_item_id)
}

pub(crate) fn missing_steering_entries_after(
    state: &CoreAgentState,
    last_entry_id: u64,
) -> Result<Vec<ContextEntry>, DomainError> {
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

    context_entries_after(state, last_entry_id, inputs)
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
        CoreAgentEvent::Context(Event::EntriesRemoved {
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
        .find(|entry| is_provider_compaction_entry(entry) && !is_standalone_prefix(entry))
}

fn is_provider_compaction_entry(entry: &ContextEntry) -> bool {
    match entry.content.provider_kind.as_deref() {
        // OpenAI Responses returns an opaque encrypted compaction item.
        Some(OPENAI_RESPONSES_COMPACTION_PROVIDER_KIND) => {
            matches!(entry.kind, ContextEntryKind::ProviderOpaque)
        }
        // Anthropic returns a native block for provider-triggered compaction
        // and a plain-text message for standalone summarization.
        Some(ANTHROPIC_MESSAGES_COMPACTION_PROVIDER_KIND) => {
            matches!(
                entry.kind,
                ContextEntryKind::Message { .. } | ContextEntryKind::ProviderOpaque
            )
        }
        Some(OPENAI_COMPLETIONS_COMPACTION_PROVIDER_KIND) => {
            matches!(entry.kind, ContextEntryKind::Message { .. })
        }
        _ => false,
    }
}

fn is_provider_compaction_prunable_entry(state: &CoreAgentState, entry: &ContextEntry) -> bool {
    if validate_entry_is_not_unconsumed_active_run_input(state, entry.entry_id).is_err() {
        return false;
    }
    is_compactable_entry(state, entry)
}

fn has_active_nonterminal_tool_batch(state: &CoreAgentState) -> bool {
    state.runs.active.as_ref().is_some_and(|active_run| {
        active_run
            .tool_batches
            .values()
            .any(|batch| batch.calls.iter().any(|call| !call.status.is_terminal()))
    })
}

pub(crate) fn entry_by_id(
    state: &CoreAgentState,
    entry_id: ContextEntryId,
) -> Option<&ContextEntry> {
    state
        .context
        .entries
        .iter()
        .find(|entry| entry.entry_id == entry_id)
}

/// The current entry for a key: the newest, since superseded catalog
/// versions stay active under the same key.
fn current_key_entry<'a>(
    state: &'a CoreAgentState,
    key: &ContextEntryKey,
) -> Option<&'a ContextEntry> {
    state
        .context
        .entries
        .iter()
        .rev()
        .find(|entry| entry.key.as_ref() == Some(key))
}

/// The current (newest) active entry under `key`, if any. Superseded catalog
/// versions share the key and stay active; callers that compare a fresh
/// snapshot against "what is published" must use this, never the first
/// entry with the key.
pub fn current_context_entry<'a>(
    state: &'a CoreAgentState,
    key: &ContextEntryKey,
) -> Option<&'a ContextEntry> {
    current_key_entry(state, key)
}

/// True when a newer active entry records `supersedes == entry_id`.
pub fn is_superseded_context_entry(state: &CoreAgentState, entry_id: ContextEntryId) -> bool {
    state
        .context
        .entries
        .iter()
        .any(|entry| entry.supersedes == Some(entry_id))
}

/// Current keyed catalog inputs, independent of the publishing subsystem.
/// Later versions replace earlier ones in the map; history remains in context.
pub fn current_catalog_inputs(
    state: &CoreAgentState,
) -> std::collections::BTreeMap<ContextEntryKey, ContextEntryInput> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::Catalog { .. }))
        .filter_map(|entry| {
            entry
                .key
                .clone()
                .map(|key| (key, context_entry_input_from_active(entry)))
        })
        .collect()
}

pub(crate) fn entries_applied_proposal(
    state: &CoreAgentState,
    run_id: RunId,
    entries: Vec<ContextEntry>,
) -> CoreAgentEventProposal {
    CoreAgentEventProposal::new(
        CoreAgentJoins {
            run_id: Some(run_id),
            ..CoreAgentJoins::default()
        },
        CoreAgentEvent::Context(Event::EntriesApplied {
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
            reason: _,
        } => {
            validate_base_revision(state, *base_revision)?;
            validate_entries_removable(state, entry_ids)?;
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
        Event::EntriesReplaced {
            base_revision,
            entries,
        } => {
            validate_base_revision(state, *base_revision)?;
            if entries.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "context entry replacement event must contain at least one entry".into(),
                ));
            }
            let mut seen = BTreeSet::new();
            for entry in entries {
                if !seen.insert(entry.entry_id) {
                    return Err(DomainError::InvariantViolation(format!(
                        "duplicate context entry replacement {}",
                        entry.entry_id
                    )));
                }
                validate_recorded_entry_replacement(state, entry)?;
            }
            for entry in entries {
                let active = state
                    .context
                    .entries
                    .iter_mut()
                    .find(|active| active.entry_id == entry.entry_id)
                    .expect("validated active entry");
                *active = entry.clone();
            }
            bump_context_revision(state)?;
            Ok(())
        }
        Event::CompactionRequested {
            base_revision,
            trigger,
            plan,
        } => {
            validate_base_revision(state, *base_revision)?;
            if !compaction_safe_boundary(state)
                || plan.trigger != *trigger
                || plan.run_id != state.runs.active.as_ref().map(|run| run.run_id)
                || plan.covered_entry_ids.is_empty()
            {
                return Err(DomainError::InvariantViolation(
                    "invalid compaction plan or execution boundary".into(),
                ));
            }
            // A recorded plan is a fact: which entries it covers is the planner's
            // decision, and the planner may change. It only has to name entries
            // that exist and that compaction is allowed to rewrite.
            for entry_id in &plan.covered_entry_ids {
                if entry_by_id(state, *entry_id).is_none() {
                    return Err(DomainError::InvariantViolation(format!(
                        "compaction plan covers unknown context entry {entry_id}"
                    )));
                }
                validate_entry_is_not_unconsumed_active_run_input(state, *entry_id)?;
            }
            if let Some(run) = state.runs.active.as_mut()
                && let Some(turn) = run.turns.values().next_back()
                && matches!(turn.outcome, Some(crate::TurnOutcome::ContextLimit { .. }))
            {
                run.context_recovery.attempts += 1;
                run.context_recovery.recovered_turn_id = Some(turn.turn_id);
            }
            if *trigger == ContextCompactionTrigger::HighWatermark
                && state.context.entries.len() >= ENTRY_GUARD_LIMIT
                && let Some(run) = state.runs.active.as_mut()
            {
                run.entry_guard_attempted = true;
            }
            state.context.compaction.phase = ContextCompactionPhase::Pending(plan.clone());
            bump_context_revision(state)?;
            Ok(())
        }
        Event::CompactionQueued { base_revision } => {
            validate_base_revision(state, *base_revision)?;
            if state.context.compaction.is_pending() || state.context.compaction.is_queued() {
                return Err(DomainError::InvariantViolation(
                    "context compaction is already pending".into(),
                ));
            }
            state.context.compaction.phase = ContextCompactionPhase::QueuedManual;
            Ok(())
        }
        Event::CompactionFinished {
            usage,
            calls: _,
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
            let queued = state.context.compaction.is_queued();
            if !state.context.compaction.is_pending() && !queued {
                return Err(DomainError::InvariantViolation(
                    "context compaction finished without a pending request".to_owned(),
                ));
            }
            if let Some(usage) = usage
                && let Some(run) = state.runs.active.as_mut()
            {
                crate::core::components::turn::accumulate_usage(&mut run.usage, usage);
            }
            let manual = queued
                || state
                    .context
                    .compaction
                    .pending_plan()
                    .is_some_and(|plan| plan.trigger == ContextCompactionTrigger::Manual);
            match status {
                ContextCompactionStatus::Succeeded => {
                    state.context.compaction.failure_floor = None;
                    // A long run that gets back under may need the guard again later.
                    // One that stays over has no use for another attempt.
                    if state.context.entries.len() < ENTRY_GUARD_LIMIT
                        && let Some(run) = state.runs.active.as_mut()
                    {
                        run.entry_guard_attempted = false;
                    }
                }
                // A plan whose run has ended was abandoned, which says nothing about
                // whether compaction works.
                ContextCompactionStatus::Failed => {
                    let attempting_run = state
                        .runs
                        .active
                        .as_ref()
                        .filter(|run| run.status == RunStatus::Active)
                        .map(|run| run.run_id);
                    if let Some(floor) = state
                        .context
                        .compaction
                        .pending_plan()
                        .filter(|plan| {
                            plan.trigger == ContextCompactionTrigger::HighWatermark
                                && plan.run_id == attempting_run
                        })
                        .map(|plan| CompactionFailureFloor {
                            tokens: plan.estimated_tokens,
                            entries: state.context.entries.len(),
                        })
                    {
                        state.context.compaction.failure_floor = Some(floor);
                    }
                }
            }
            state.context.compaction.phase = ContextCompactionPhase::Idle;
            bump_context_revision(state)?;
            state.context.compaction.last_finished_revision = Some(state.context.revision);
            if manual {
                state.context.compaction.last_manual_finished_revision =
                    Some(state.context.revision);
            }
            Ok(())
        }
    }
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
        if let Some(last) = state.context.entries.last()
            && entry.entry_id <= last.entry_id
        {
            return Err(DomainError::InvariantViolation(format!(
                "context entry id {} must be greater than last active entry id {}",
                entry.entry_id, last.entry_id
            )));
        }

        record_entry_materialization(state, entry)?;

        if let Some(key) = entry.key.as_ref() {
            let expected = supersede_target(state, key, &entry.kind);
            if entry.supersedes != expected {
                return Err(DomainError::InvariantViolation(format!(
                    "context entry {} supersedes {:?} but key {} currently holds {:?}",
                    entry.entry_id, entry.supersedes, key, expected
                )));
            }
            if expected.is_none() {
                remove_context_entry_by_key(state, key);
            }
        }

        state.context.entries.push(entry.clone());
        state.id_cursors.last_context_item_id = entry.entry_id.as_u64();

        if let Some(key) = entry.key.as_ref()
            && entry.supersedes.is_some()
        {
            drop_superseded_beyond_cap(state, key);
        }
    }
    Ok(())
}

/// Keep at most `SUPERSEDED_CATALOG_CAP` superseded versions under a key,
/// dropping the oldest. Superseded catalogs are never run input, so no
/// consumption check applies.
fn drop_superseded_beyond_cap(state: &mut CoreAgentState, key: &ContextEntryKey) {
    let mut versions = state
        .context
        .entries
        .iter()
        .filter(|entry| entry.key.as_ref() == Some(key))
        .map(|entry| entry.entry_id)
        .collect::<Vec<_>>();
    // The newest is current; everything before it is superseded.
    versions.pop();
    if versions.len() <= SUPERSEDED_CATALOG_CAP {
        return;
    }
    let excess = versions.len() - SUPERSEDED_CATALOG_CAP;
    let dropped = versions.into_iter().take(excess).collect::<BTreeSet<_>>();
    state
        .context
        .entries
        .retain(|entry| !dropped.contains(&entry.entry_id));
}

fn validate_no_duplicate_entry_keys(entries: &[ContextEntry]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        if let Some(key) = entry.key.as_ref()
            && !seen.insert(key.clone())
        {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate context key {} in entries event",
                key
            )));
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
            let RunSource::Input { input } = &active_run.source;
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
        | ContextEntrySource::ApprovalDecision { .. }
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
        || entry.content != input.content
        || entry.preview != input.preview
        || entry.origin != input.origin
        || entry.provenance_ref != input.provenance_ref
        || entry.token_estimate != input.token_estimate
    {
        return Err(DomainError::InvariantViolation(format!(
            "context entry {} does not match accepted input payload",
            entry.entry_id
        )));
    }
    Ok(())
}

/// `input` as the in-place replacement of active entry `entry_id`, keeping
/// the entry's id, key, and source. `None` when the entry is not active.
pub fn replacement_entry(
    state: &CoreAgentState,
    entry_id: ContextEntryId,
    input: ContextEntryInput,
) -> Option<ContextEntry> {
    let active = entry_by_id(state, entry_id)?;
    Some(input.commit(
        entry_id,
        active.key.clone(),
        active.source.clone(),
        active.supersedes,
    ))
}

/// Refresh discovery metadata without changing a catalog's rendered message.
pub(crate) fn catalog_metadata_replacement(
    state: &CoreAgentState,
    key: &ContextEntryKey,
    input: &ContextEntryInput,
) -> Option<ContextEntry> {
    let current = current_key_entry(state, key)?;
    if !is_supersedable_catalog_kind(&current.kind)
        || current.kind != input.kind
        || current.content != input.content
    {
        return None;
    }
    replacement_entry(state, current.entry_id, input.clone())
}

/// A replacement may change only content: the kind (role, call id) must stay
/// the same, so a tool call keeps its answer. Only tool results and user
/// messages qualify for content edits. Current keyed catalogs permit metadata
/// edits only, preserving content and supersession. Tool calls, assistant output,
/// reasoning, and provider-opaque entries carry provider-shaped content.
pub fn validate_entry_replacement(
    state: &CoreAgentState,
    entry: &ContextEntry,
) -> Result<(), DomainError> {
    validate_recorded_entry_replacement(state, entry)?;
    let entry_id = entry.entry_id;
    let active = entry_by_id(state, entry_id).expect("validated active entry");
    let catalog_metadata_only = is_supersedable_catalog_kind(&active.kind)
        && active.content == entry.content
        && active.supersedes == entry.supersedes
        && active.key.as_ref().is_some_and(|key| {
            current_key_entry(state, key).is_some_and(|current| current.entry_id == entry_id)
        });
    let replaceable = catalog_metadata_only
        || matches!(
            active.kind,
            ContextEntryKind::ToolResult { .. }
                | ContextEntryKind::Message {
                    role: ContextMessageRole::User
                }
        );
    if !replaceable {
        return Err(DomainError::InvariantViolation(format!(
            "context entry {entry_id} cannot be replaced: only tool results, user messages, and current catalog metadata can"
        )));
    }
    Ok(())
}

/// What any replacement must keep true, whichever kinds admission allows.
fn validate_recorded_entry_replacement(
    state: &CoreAgentState,
    entry: &ContextEntry,
) -> Result<(), DomainError> {
    let entry_id = entry.entry_id;
    let Some(active) = entry_by_id(state, entry_id) else {
        return Err(DomainError::InvariantViolation(format!(
            "cannot replace unknown context entry {entry_id}"
        )));
    };
    if entry.kind != active.kind || entry.key != active.key || entry.source != active.source {
        return Err(DomainError::InvariantViolation(format!(
            "replacement of context entry {entry_id} must keep its kind, key, and source"
        )));
    }
    validate_entry_is_not_unconsumed_active_run_input(state, entry_id)
}

fn validate_entries_removable(
    state: &CoreAgentState,
    entry_ids: &[ContextEntryId],
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
        content: entry.content.clone(),
        preview: entry.preview.clone(),
        origin: entry.origin.clone(),
        provenance_ref: entry.provenance_ref.clone(),
        token_estimate: entry.token_estimate.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_entry(id: u64, kind: ContextEntryKind, source: ContextEntrySource) -> ContextEntry {
        ContextEntry {
            origin: None,
            entry_id: ContextEntryId::new(id),
            key: None,
            kind,
            source,
            content: crate::ContentRef {
                content_ref: BlobRef::from_bytes(b"entry"),
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }
    }

    fn output_source(turn: u64) -> ContextEntrySource {
        ContextEntrySource::AssistantOutput {
            run_id: RunId::new(1),
            turn_id: TurnId::new(turn),
        }
    }

    fn call(id: u64, call_id: &str, turn: u64) -> ContextEntry {
        rendered_entry(
            id,
            ContextEntryKind::ToolCall {
                call_id: crate::ToolCallId::new(call_id),
                name: crate::ToolName::new("tool"),
            },
            output_source(turn),
        )
    }

    fn result(id: u64, call_id: &str, turn: u64) -> ContextEntry {
        rendered_entry(
            id,
            ContextEntryKind::ToolResult {
                call_id: crate::ToolCallId::new(call_id),
                is_error: false,
            },
            ContextEntrySource::Tool {
                run_id: RunId::new(1),
                turn_id: TurnId::new(turn),
                batch_id: None,
            },
        )
    }

    fn user_note(id: u64) -> ContextEntry {
        rendered_entry(
            id,
            ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            ContextEntrySource::Runtime {
                label: "note".to_owned(),
            },
        )
    }

    fn media(id: u64, turn: u64) -> ContextEntry {
        ContextEntry {
            source: ContextEntrySource::Tool {
                run_id: RunId::new(1),
                turn_id: TurnId::new(turn),
                batch_id: None,
            },
            ..user_note(id)
        }
    }

    fn reasoning(id: u64, turn: u64) -> ContextEntry {
        rendered_entry(
            id,
            ContextEntryKind::ReasoningState,
            ContextEntrySource::Reasoning {
                run_id: RunId::new(1),
                turn_id: TurnId::new(turn),
            },
        )
    }

    fn answer(id: u64, turn: u64) -> ContextEntry {
        rendered_entry(
            id,
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            },
            output_source(turn),
        )
    }

    fn render_order(entries: Vec<ContextEntry>) -> Vec<u64> {
        tool_groups_adjacent(entries.iter())
            .into_iter()
            .map(|id| id.as_u64())
            .collect()
    }

    #[test]
    fn a_session_with_nothing_interleaved_renders_in_arrival_order() {
        assert_eq!(
            render_order(vec![
                user_note(1),
                reasoning(2, 1),
                call(3, "a", 1),
                call(4, "b", 1),
                result(5, "a", 1),
                result(6, "b", 1),
                media(7, 1),
                answer(8, 2),
            ]),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn entries_arriving_inside_a_group_render_behind_its_last_result() {
        assert_eq!(
            render_order(vec![
                call(1, "a", 1),
                call(2, "b", 1),
                user_note(3),
                result(4, "a", 1),
                media(5, 1),
                user_note(6),
                result(7, "b", 1),
                media(8, 1),
                answer(9, 2),
            ]),
            [1, 2, 4, 7, 3, 5, 6, 8, 9]
        );
    }

    #[test]
    fn the_calling_turns_output_stays_between_its_calls() {
        assert_eq!(
            render_order(vec![
                reasoning(1, 1),
                call(2, "a", 1),
                reasoning(3, 1),
                call(4, "b", 1),
                answer(5, 1),
                user_note(6),
                result(7, "b", 1),
                result(8, "a", 1),
            ]),
            [1, 2, 3, 4, 5, 7, 8, 6]
        );
    }

    #[test]
    fn a_later_turn_closes_a_group_that_was_never_answered() {
        // The later turn's request already carried the note at its position.
        assert_eq!(
            render_order(vec![
                call(1, "gone", 1),
                user_note(2),
                answer(3, 2),
                call(4, "a", 3),
                user_note(5),
                result(6, "a", 3),
            ]),
            [1, 2, 3, 4, 6, 5]
        );
    }

    #[test]
    fn a_call_never_answered_at_the_tail_keeps_arrival_order() {
        assert_eq!(
            render_order(vec![call(1, "a", 1), user_note(2), user_note(3)]),
            [1, 2, 3]
        );
    }

    #[test]
    fn a_result_answering_no_open_call_is_an_ordinary_entry() {
        assert_eq!(
            render_order(vec![
                result(1, "compacted", 1),
                call(2, "a", 2),
                result(3, "compacted", 1),
                result(4, "a", 2),
            ]),
            [1, 2, 4, 3]
        );
    }

    #[test]
    fn a_provider_reusing_a_call_id_in_a_later_turn_gets_a_new_group() {
        assert_eq!(
            render_order(vec![
                call(1, "call_0", 1),
                result(2, "call_0", 1),
                call(3, "call_0", 2),
                user_note(4),
                result(5, "call_0", 2),
            ]),
            [1, 2, 3, 5, 4]
        );
    }

    #[test]
    fn compaction_requests_and_pending_snapshots_require_a_plan() {
        let plan = ContextCompactionPlan {
            run_id: None,
            covered_entry_ids: vec![ContextItemId::new(1)],
            trigger: ContextCompactionTrigger::Manual,
            estimated_tokens: None,
        };
        let event = Event::CompactionRequested {
            base_revision: 0,
            trigger: plan.trigger,
            plan: plan.clone(),
        };
        let mut encoded = serde_json::to_value(&event).unwrap();
        encoded["compaction_requested"]
            .as_object_mut()
            .unwrap()
            .remove("plan");
        assert!(serde_json::from_value::<Event>(encoded).is_err());
        assert!(
            serde_json::from_value::<ContextCompactionPhase>(serde_json::json!({"pending": null}))
                .is_err()
        );
        let phase = ContextCompactionPhase::Pending(plan);
        assert_eq!(
            serde_json::from_value::<ContextCompactionPhase>(serde_json::to_value(&phase).unwrap())
                .unwrap(),
            phase
        );
    }
}
