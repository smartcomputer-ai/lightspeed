use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActiveRun, BlobRef, ContextEvent, ContextItem, ContextItemId, ContextItemKind,
    ContextItemSource, CoreAgentEventKind, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, PlanNext, PlanningError, ProviderApiKind, RunId, RunStatus,
    ToolBatchId, ToolCallId, ToolName, ToolProfileId, TurnId, TurnOutcome, TurnStatus,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigEvent {
    RegistryChanged { registry: ToolRegistry },
    ProfileSelected { profile_id: ToolProfileId },
    DefaultTargetSet { target: ToolExecutionTarget },
    DefaultTargetCleared { namespace: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    BatchStarted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        calls: Vec<ObservedToolCall>,
    },
    CallStarted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        call_id: ToolCallId,
        tool_name: ToolName,
        arguments_ref: BlobRef,
        execution_target: Option<ToolExecutionTarget>,
    },
    CallCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        result: ToolCallResult,
    },
    BatchCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
    },
}

pub type ToolConfigEvent = ConfigEvent;
pub type ToolEvent = Event;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreToolPlanner;

impl PlanNext for CoreToolPlanner {
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
        if active_run.status != RunStatus::Active || active_run.active_turn_id.is_some() {
            return Ok(Vec::new());
        }

        if let Some(batch_id) = active_run.active_tool_batch_id {
            let proposals = decide_active_tool_batch_invocations(state, active_run, batch_id)?;
            if proposals.is_empty() {
                return decide_active_tool_batch_completion(state, active_run, batch_id);
            }
            return Ok(proposals);
        }

        for (turn_id, turn) in &active_run.turns {
            if turn.status != TurnStatus::Completed
                || turn.outcome.as_ref() != Some(&TurnOutcome::ToolCallsQueued)
            {
                continue;
            }
            if active_run
                .tool_batches
                .values()
                .any(|batch| batch.turn_id == *turn_id)
                || active_run
                    .completed_tool_batches
                    .values()
                    .any(|batch| batch.turn_id == *turn_id)
            {
                continue;
            }
            let Some(facts) = turn.facts.as_ref() else {
                return Err(DomainError::InvariantViolation(format!(
                    "completed tool-call turn {} is missing generation facts",
                    turn_id
                ))
                .into());
            };
            if facts.tool_calls.is_empty() {
                continue;
            }

            let next_batch_id = state
                .id_cursors
                .last_tool_batch_id
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::InvariantViolation("tool batch id cursor exhausted".to_owned())
                })?;
            let batch_id = ToolBatchId::new(next_batch_id);
            let joins = CoreAgentJoins {
                run_id: Some(active_run.run_id),
                turn_id: Some(*turn_id),
                tool_batch_id: Some(batch_id),
                ..CoreAgentJoins::default()
            };
            return Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEventKind::Tool(Event::BatchStarted {
                    run_id: active_run.run_id,
                    turn_id: *turn_id,
                    batch_id,
                    calls: facts.tool_calls.clone(),
                }),
            )]);
        }

        Ok(Vec::new())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolingState {
    pub registry: ToolRegistry,
    pub selected_profile_id: Option<ToolProfileId>,
    pub routing: ToolRoutingState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRegistry {
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub profiles: BTreeMap<ToolProfileId, ToolProfile>,
}

impl ToolRegistry {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (tool_name, tool) in &self.tools {
            if &tool.name != tool_name {
                return Err(DomainError::InvariantViolation(format!(
                    "tool registry key {} does not match tool name {}",
                    tool_name, tool.name
                )));
            }
        }

        for (profile_id, profile) in &self.profiles {
            if &profile.profile_id != profile_id {
                return Err(DomainError::InvariantViolation(format!(
                    "tool profile key {} does not match profile id {}",
                    profile_id, profile.profile_id
                )));
            }
            for tool_name in &profile.visible_tools {
                if !self.tools.contains_key(tool_name) {
                    return Err(DomainError::InvariantViolation(format!(
                        "tool profile {} references missing tool {}",
                        profile_id, tool_name
                    )));
                }
            }
            if let Some(ToolChoice {
                mode: ToolChoiceMode::Specific { tool_name },
                ..
            }) = &profile.tool_choice
            {
                if !profile.visible_tools.iter().any(|name| name == tool_name) {
                    return Err(DomainError::InvariantViolation(format!(
                        "tool profile {} chooses non-visible tool {}",
                        profile_id, tool_name
                    )));
                }
            }
        }

        for tool in self.tools.values() {
            tool.target_requirement.validate()?;
        }

        Ok(())
    }
}

pub(crate) fn validate_registry_keeps_active_profile(
    state: &CoreAgentState,
    registry: &ToolRegistry,
) -> Result<(), DomainError> {
    let profile_id = state.tooling.selected_profile_id.as_ref().or_else(|| {
        state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.tool_profile_id.as_ref())
    });
    if let Some(profile_id) = profile_id {
        validate_profile_exists(registry, profile_id)?;
    }
    Ok(())
}

pub(crate) fn validate_profile_exists(
    registry: &ToolRegistry,
    profile_id: &ToolProfileId,
) -> Result<(), DomainError> {
    if registry.profiles.contains_key(profile_id) {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "tool profile {} does not exist",
            profile_id
        )))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProfile {
    pub profile_id: ToolProfileId,
    pub visible_tools: Vec<ToolName>,
    pub tool_choice: Option<ToolChoice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: ToolName,
    pub kind: ToolKind,
    pub parallelism: ToolParallelism,
    #[serde(default)]
    pub target_requirement: ToolTargetRequirement,
}

impl ToolSpec {
    pub fn invokes_client_effect(&self) -> bool {
        match &self.kind {
            ToolKind::Function(_) => true,
            ToolKind::ProviderNative(native) => {
                native.execution == ProviderNativeToolExecution::ClientEffect
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRoutingState {
    pub default_targets: BTreeMap<String, ToolExecutionTarget>,
}

impl ToolRoutingState {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (namespace, target) in &self.default_targets {
            validate_target_namespace(namespace)?;
            target.validate()?;
            if target.namespace != *namespace {
                return Err(DomainError::InvariantViolation(format!(
                    "default target namespace {} does not match target namespace {}",
                    namespace, target.namespace
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionTarget {
    pub namespace: String,
    pub id: String,
}

impl ToolExecutionTarget {
    pub fn new(namespace: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            id: id.into(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_target_namespace(&self.namespace)?;
        validate_target_id(&self.id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTargetRequirement {
    #[default]
    None,
    Optional {
        namespace: String,
    },
    Required {
        namespace: String,
    },
}

impl ToolTargetRequirement {
    pub fn optional(namespace: impl Into<String>) -> Self {
        Self::Optional {
            namespace: namespace.into(),
        }
    }

    pub fn required(namespace: impl Into<String>) -> Self {
        Self::Required {
            namespace: namespace.into(),
        }
    }

    pub fn namespace(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Optional { namespace } | Self::Required { namespace } => Some(namespace),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if let Some(namespace) = self.namespace() {
            validate_target_namespace(namespace)?;
        }
        Ok(())
    }
}

pub(crate) fn validate_default_tool_target_set(
    target: &ToolExecutionTarget,
) -> Result<(), DomainError> {
    target.validate()
}

pub(crate) fn validate_default_tool_target_clear(namespace: &str) -> Result<(), DomainError> {
    let requirement = ToolTargetRequirement::required(namespace.to_owned());
    requirement.validate()
}

pub(crate) fn validate_tool_execution_target_for_requirement(
    requirement: &ToolTargetRequirement,
    target: Option<&ToolExecutionTarget>,
) -> Result<(), DomainError> {
    requirement.validate()?;
    if let Some(target) = target {
        target.validate()?;
    }
    match (requirement, target) {
        (ToolTargetRequirement::None, None) => Ok(()),
        (ToolTargetRequirement::None, Some(_)) => Err(DomainError::InvariantViolation(
            "tool invocation target is not allowed for this tool".into(),
        )),
        (ToolTargetRequirement::Optional { namespace }, None) => {
            ToolTargetRequirement::required(namespace.clone()).validate()
        }
        (ToolTargetRequirement::Optional { namespace }, Some(target))
        | (ToolTargetRequirement::Required { namespace }, Some(target)) => {
            if target.namespace == *namespace {
                Ok(())
            } else {
                Err(DomainError::InvariantViolation(format!(
                    "tool invocation target namespace {} does not match required namespace {}",
                    target.namespace, namespace
                )))
            }
        }
        (ToolTargetRequirement::Required { namespace }, None) => {
            Err(DomainError::InvariantViolation(format!(
                "tool invocation requires execution target namespace {}",
                namespace
            )))
        }
    }
}

pub(crate) fn validate_unique_tool_call_ids(calls: &[ObservedToolCall]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for call in calls {
        if !seen.insert(call.call_id.clone()) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate tool call id {}",
                call.call_id
            )));
        }
    }
    Ok(())
}

const TARGET_COMPONENT_MAX_LEN: usize = 128;

fn validate_target_namespace(namespace: &str) -> Result<(), DomainError> {
    validate_target_component(
        "tool execution target namespace",
        namespace,
        "ASCII letters, digits, '_', '-', '.'",
        |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'),
    )
}

fn validate_target_id(id: &str) -> Result<(), DomainError> {
    validate_target_component(
        "tool execution target id",
        id,
        "ASCII letters, digits, '_', '-', '.', ':'",
        |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'),
    )
}

fn validate_target_component(
    kind: &'static str,
    value: &str,
    allowed: &'static str,
    allowed_char: impl Fn(char) -> bool,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} must not be empty"
        )));
    }
    if value.len() > TARGET_COMPONENT_MAX_LEN {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} is too long: {} bytes, max {}",
            value.len(),
            TARGET_COMPONENT_MAX_LEN
        )));
    }
    let Some(first) = value.chars().next() else {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} must not be empty"
        )));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} must start with an ASCII letter or digit"
        )));
    }
    for (index, ch) in value.char_indices() {
        if !allowed_char(ch) {
            return Err(DomainError::InvariantViolation(format!(
                "{kind} contains invalid character {ch:?} at byte {index}; allowed: {allowed}"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Function(FunctionToolSpec),
    ProviderNative(ProviderNativeToolSpec),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionToolSpec {
    pub model_name: Option<ToolName>,
    pub description_ref: Option<BlobRef>,
    pub input_schema_ref: BlobRef,
    pub output_schema_ref: Option<BlobRef>,
    pub strict: Option<bool>,
    pub provider_options_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderNativeToolSpec {
    pub api_kind: ProviderApiKind,
    pub native_tool_ref: BlobRef,
    pub execution: ProviderNativeToolExecution,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeToolExecution {
    ProviderHosted,
    ClientEffect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolChoice {
    pub mode: ToolChoiceMode,
    pub disable_parallel_tool_use: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoiceMode {
    Auto,
    None,
    RequiredAny,
    Specific { tool_name: ToolName },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolParallelism {
    Exclusive,
    ParallelSafe,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedToolCall {
    pub call_id: ToolCallId,
    pub tool_name: ToolName,
    pub provider_kind: Option<String>,
    pub arguments_ref: BlobRef,
    pub native_call_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveToolBatch {
    pub batch_id: ToolBatchId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub calls: Vec<ToolCallState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedToolBatch {
    pub batch_id: ToolBatchId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub results: Vec<ToolCallResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallState {
    pub call: ObservedToolCall,
    pub status: ToolCallStatus,
    pub execution_target: Option<ToolExecutionTarget>,
    pub result: Option<ToolCallResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Observed,
    Accepted,
    Unavailable,
    Pending,
    Succeeded,
    Failed,
    Cancelled,
}

impl ToolCallStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Succeeded | Self::Failed | Self::Cancelled
        )
    }

    pub fn is_error(self) -> bool {
        !matches!(self, Self::Succeeded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub status: ToolCallStatus,
    pub output_ref: Option<BlobRef>,
    pub model_visible_output_ref: Option<BlobRef>,
    pub error_ref: Option<BlobRef>,
}

pub(crate) fn tool_result_context_item_exists(
    state: &CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    result: &ToolCallResult,
) -> bool {
    let Some(expected_ref) = tool_result_context_ref(result) else {
        return false;
    };
    state.context.retained_items.iter().any(|item| {
        matches!(
            (&item.source, &item.kind),
            (
                crate::ContextItemSource::ToolResult {
                    run_id: item_run_id,
                    turn_id: item_turn_id,
                },
                crate::ContextItemKind::ToolResult {
                    call_id: item_call_id,
                    is_error,
                },
            ) if *item_run_id == run_id
                && *item_turn_id == turn_id
                && item_call_id == &result.call_id
                && *is_error == result.status.is_error()
                && item.native_item_ref == *expected_ref
        )
    })
}

pub(crate) fn tool_result_context_ref(result: &ToolCallResult) -> Option<&BlobRef> {
    result
        .model_visible_output_ref
        .as_ref()
        .or(result.error_ref.as_ref())
        .or(result.output_ref.as_ref())
}

fn decide_active_tool_batch_invocations(
    state: &CoreAgentState,
    active_run: &ActiveRun,
    batch_id: ToolBatchId,
) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active tool batch {} is missing", batch_id))
    })?;
    if batch.run_id != active_run.run_id {
        return Err(DomainError::InvariantViolation(format!(
            "active tool batch {} run id {} does not match active run {}",
            batch_id, batch.run_id, active_run.run_id
        ))
        .into());
    }

    let mut proposals = Vec::new();
    for call_state in &batch.calls {
        if call_state.status != ToolCallStatus::Accepted {
            continue;
        }
        let Some(tool) = state.tooling.registry.tools.get(&call_state.call.tool_name) else {
            return Err(DomainError::InvariantViolation(format!(
                "accepted tool call references missing tool {}",
                call_state.call.tool_name
            ))
            .into());
        };
        if !tool.invokes_client_effect() {
            continue;
        }
        let execution_target = resolve_tool_execution_target(state, tool)?;

        let joins = CoreAgentJoins {
            run_id: Some(batch.run_id),
            turn_id: Some(batch.turn_id),
            tool_batch_id: Some(batch.batch_id),
            tool_call_id: Some(call_state.call.call_id.clone()),
            ..CoreAgentJoins::default()
        };
        proposals.push(CoreAgentEventProposal::new(
            joins,
            CoreAgentEventKind::Tool(Event::CallStarted {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
                batch_id: batch.batch_id,
                call_id: call_state.call.call_id.clone(),
                tool_name: call_state.call.tool_name.clone(),
                arguments_ref: call_state.call.arguments_ref.clone(),
                execution_target,
            }),
        ));
    }

    Ok(proposals)
}

fn decide_active_tool_batch_completion(
    state: &CoreAgentState,
    active_run: &ActiveRun,
    batch_id: ToolBatchId,
) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active tool batch {} is missing", batch_id))
    })?;
    if !batch
        .calls
        .iter()
        .all(|call_state| call_state.status.is_terminal())
    {
        return Ok(Vec::new());
    }

    let mut proposals = Vec::new();
    let result_items = tool_result_context_items(state, batch)?;
    let joins = CoreAgentJoins {
        run_id: Some(batch.run_id),
        turn_id: Some(batch.turn_id),
        tool_batch_id: Some(batch.batch_id),
        ..CoreAgentJoins::default()
    };
    if !result_items.is_empty() {
        proposals.push(CoreAgentEventProposal::new(
            joins.clone(),
            CoreAgentEventKind::Context(ContextEvent::ItemsRecorded {
                items: result_items,
            }),
        ));
    }
    proposals.push(CoreAgentEventProposal::new(
        joins,
        CoreAgentEventKind::Tool(Event::BatchCompleted {
            run_id: batch.run_id,
            turn_id: batch.turn_id,
            batch_id: batch.batch_id,
        }),
    ));
    Ok(proposals)
}

fn tool_result_context_items(
    state: &CoreAgentState,
    batch: &ActiveToolBatch,
) -> Result<Vec<ContextItem>, PlanningError> {
    let mut next_item_id = state.id_cursors.last_context_item_id;
    let mut items = Vec::new();
    for call_state in &batch.calls {
        let Some(result) = call_state.result.as_ref() else {
            return Err(DomainError::InvariantViolation(
                "terminal tool call is missing result".to_owned(),
            )
            .into());
        };
        if result.call_id != call_state.call.call_id || result.status != call_state.status {
            return Err(DomainError::InvariantViolation(
                "terminal tool call result does not match call state".to_owned(),
            )
            .into());
        }
        if tool_result_context_item_exists(state, batch.run_id, batch.turn_id, result) {
            continue;
        }
        let native_item_ref = tool_result_context_ref(result).cloned().ok_or_else(|| {
            DomainError::InvariantViolation(
                "terminal tool result is missing a model-visible ref".to_owned(),
            )
        })?;
        next_item_id = next_item_id.checked_add(1).ok_or_else(|| {
            DomainError::InvariantViolation("context item id cursor exhausted".to_owned())
        })?;
        items.push(ContextItem {
            item_id: ContextItemId::new(next_item_id),
            kind: ContextItemKind::ToolResult {
                call_id: result.call_id.clone(),
                is_error: result.status.is_error(),
            },
            source: ContextItemSource::ToolResult {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
            },
            native_item_ref,
            media_type: None,
            preview: None,
            provider_kind: call_state.call.provider_kind.clone(),
            provider_item_id: None,
            token_estimate: None,
        });
    }
    Ok(items)
}

fn resolve_tool_execution_target(
    state: &CoreAgentState,
    tool: &ToolSpec,
) -> Result<Option<ToolExecutionTarget>, PlanningError> {
    tool.target_requirement.validate()?;
    match &tool.target_requirement {
        ToolTargetRequirement::None => Ok(None),
        ToolTargetRequirement::Optional { namespace } => Ok(state
            .tooling
            .routing
            .default_targets
            .get(namespace)
            .cloned()),
        ToolTargetRequirement::Required { namespace } => state
            .tooling
            .routing
            .default_targets
            .get(namespace)
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "tool {} requires default execution target namespace {}",
                    tool.name, namespace
                ))
                .into()
            }),
    }
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::BatchStarted {
            run_id,
            turn_id,
            batch_id,
            calls,
        } => {
            if calls.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "tool batch must contain at least one call".into(),
                ));
            }
            validate_unique_tool_call_ids(calls)?;
            let expected_batch_id = state
                .id_cursors
                .last_tool_batch_id
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::InvariantViolation("tool batch id cursor exhausted".into())
                })?;
            if batch_id.as_u64() != expected_batch_id {
                return Err(DomainError::InvariantViolation(format!(
                    "expected tool batch id {}, got {}",
                    expected_batch_id, batch_id
                )));
            }
            let call_states = calls
                .iter()
                .map(|call| initial_tool_call_state(state, call))
                .collect::<Vec<_>>();
            {
                let active_run = crate::core::components::run::active_run_mut(state, *run_id)?;
                if active_run.status != RunStatus::Active {
                    return Err(DomainError::InvariantViolation(
                        "tool batches can only start for active runs".into(),
                    ));
                }
                if active_run.active_tool_batch_id.is_some() {
                    return Err(DomainError::InvariantViolation(
                        "cannot start tool batch while another batch is active".into(),
                    ));
                }
                if active_run.tool_batches.contains_key(batch_id) {
                    return Err(DomainError::InvariantViolation(format!(
                        "duplicate tool batch id {}",
                        batch_id
                    )));
                }
                if active_run.completed_tool_batches.contains_key(batch_id) {
                    return Err(DomainError::InvariantViolation(format!(
                        "duplicate completed tool batch id {}",
                        batch_id
                    )));
                }
                if active_run
                    .tool_batches
                    .values()
                    .any(|batch| batch.turn_id == *turn_id)
                    || active_run
                        .completed_tool_batches
                        .values()
                        .any(|batch| batch.turn_id == *turn_id)
                {
                    return Err(DomainError::InvariantViolation(format!(
                        "turn {} already has a tool batch",
                        turn_id
                    )));
                }
                let turn = active_run.turns.get(turn_id).ok_or_else(|| {
                    DomainError::InvariantViolation(format!(
                        "tool batch turn {} is missing",
                        turn_id
                    ))
                })?;
                if turn.status != TurnStatus::Completed
                    || turn.outcome.as_ref() != Some(&TurnOutcome::ToolCallsQueued)
                {
                    return Err(DomainError::InvariantViolation(
                        "tool batch requires completed turn with queued tool calls".into(),
                    ));
                }
                let Some(facts) = turn.facts.as_ref() else {
                    return Err(DomainError::InvariantViolation(
                        "tool batch turn is missing generation facts".into(),
                    ));
                };
                if facts.tool_calls != *calls {
                    return Err(DomainError::InvariantViolation(
                        "tool batch calls do not match generation facts".into(),
                    ));
                }
                active_run.tool_batches.insert(
                    *batch_id,
                    ActiveToolBatch {
                        batch_id: *batch_id,
                        run_id: *run_id,
                        turn_id: *turn_id,
                        calls: call_states,
                    },
                );
                active_run.active_tool_batch_id = Some(*batch_id);
            }
            state.id_cursors.last_tool_batch_id = batch_id.as_u64();
            Ok(())
        }
        Event::CallStarted {
            run_id,
            turn_id,
            batch_id,
            call_id,
            tool_name,
            arguments_ref,
            execution_target,
        } => start_tool_call(
            state,
            *run_id,
            *turn_id,
            *batch_id,
            call_id,
            tool_name,
            arguments_ref,
            execution_target.as_ref(),
        ),
        Event::CallCompleted {
            run_id,
            turn_id,
            batch_id,
            result,
        } => complete_tool_call(state, *run_id, *turn_id, *batch_id, result),
        Event::BatchCompleted {
            run_id,
            turn_id,
            batch_id,
        } => complete_tool_batch(state, *run_id, *turn_id, *batch_id),
    }
}

pub(crate) fn apply_config_event(
    state: &mut CoreAgentState,
    event: &ConfigEvent,
) -> Result<(), DomainError> {
    if state.lifecycle.status != CoreAgentStatus::Open {
        return Err(DomainError::InvariantViolation(
            "tool config can only change while session is open".into(),
        ));
    }

    match event {
        ConfigEvent::RegistryChanged { registry } => {
            registry.validate()?;
            validate_registry_keeps_active_profile(state, registry)?;
            state.tooling.registry = registry.clone();
            Ok(())
        }
        ConfigEvent::ProfileSelected { profile_id } => {
            if !state.tooling.registry.profiles.contains_key(profile_id) {
                return Err(DomainError::InvariantViolation(format!(
                    "tool profile {} does not exist",
                    profile_id
                )));
            }
            state.tooling.selected_profile_id = Some(profile_id.clone());
            Ok(())
        }
        ConfigEvent::DefaultTargetSet { target } => {
            validate_default_tool_target_set(target)?;
            state
                .tooling
                .routing
                .default_targets
                .insert(target.namespace.clone(), target.clone());
            state.tooling.routing.validate()
        }
        ConfigEvent::DefaultTargetCleared { namespace } => {
            validate_default_tool_target_clear(namespace)?;
            state.tooling.routing.default_targets.remove(namespace);
            state.tooling.routing.validate()
        }
    }
}

fn initial_tool_call_state(state: &CoreAgentState, call: &ObservedToolCall) -> ToolCallState {
    let status = initial_tool_call_status(state, call);
    let result = if status == ToolCallStatus::Unavailable {
        Some(unavailable_tool_result(call))
    } else {
        None
    };
    ToolCallState {
        call: call.clone(),
        status,
        execution_target: None,
        result,
    }
}

fn initial_tool_call_status(state: &CoreAgentState, call: &ObservedToolCall) -> ToolCallStatus {
    let Some(profile_id) = state.tooling.selected_profile_id.as_ref().or_else(|| {
        state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.tool_profile_id.as_ref())
    }) else {
        return ToolCallStatus::Unavailable;
    };
    let Some(profile) = state.tooling.registry.profiles.get(profile_id) else {
        return ToolCallStatus::Unavailable;
    };
    if !profile
        .visible_tools
        .iter()
        .any(|name| name == &call.tool_name)
    {
        return ToolCallStatus::Unavailable;
    }
    let Some(tool) = state.tooling.registry.tools.get(&call.tool_name) else {
        return ToolCallStatus::Unavailable;
    };
    if !tool.invokes_client_effect() {
        return ToolCallStatus::Unavailable;
    }
    if tool.target_requirement.validate().is_err() {
        return ToolCallStatus::Unavailable;
    }
    if let Some(namespace) = required_target_namespace(&tool.target_requirement) {
        if !state
            .tooling
            .routing
            .default_targets
            .contains_key(namespace)
        {
            return ToolCallStatus::Unavailable;
        }
    }
    ToolCallStatus::Accepted
}

fn required_target_namespace(requirement: &ToolTargetRequirement) -> Option<&str> {
    match requirement {
        ToolTargetRequirement::Required { namespace } => Some(namespace.as_str()),
        ToolTargetRequirement::None | ToolTargetRequirement::Optional { .. } => None,
    }
}

fn unavailable_tool_result(call: &ObservedToolCall) -> ToolCallResult {
    let error_ref = BlobRef::from_bytes(
        format!(
            "engine tool unavailable\ncall_id={}\ntool_name={}\n",
            call.call_id, call.tool_name
        )
        .as_bytes(),
    );
    ToolCallResult {
        call_id: call.call_id.clone(),
        status: ToolCallStatus::Unavailable,
        output_ref: None,
        model_visible_output_ref: Some(error_ref.clone()),
        error_ref: Some(error_ref),
    }
}

fn complete_tool_batch(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    batch_id: ToolBatchId,
) -> Result<(), DomainError> {
    let results = {
        let active_run = crate::core::components::run::active_run_ref(state, run_id)?;
        if active_run.active_tool_batch_id != Some(batch_id) {
            return Err(DomainError::InvariantViolation(
                "completed tool batch does not match active tool batch".into(),
            ));
        }
        if active_run.completed_tool_batches.contains_key(&batch_id) {
            return Err(DomainError::InvariantViolation(format!(
                "duplicate completed tool batch id {}",
                batch_id
            )));
        }
        let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
            DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
        })?;
        if batch.run_id != run_id || batch.turn_id != turn_id {
            return Err(DomainError::InvariantViolation(
                "completed tool batch does not match run/turn".into(),
            ));
        }

        let mut results = Vec::with_capacity(batch.calls.len());
        for call_state in &batch.calls {
            if !call_state.status.is_terminal() {
                return Err(DomainError::InvariantViolation(
                    "tool batch cannot complete before all calls are terminal".into(),
                ));
            }
            let Some(result) = call_state.result.clone() else {
                return Err(DomainError::InvariantViolation(
                    "terminal tool call is missing result".into(),
                ));
            };
            if result.call_id != call_state.call.call_id || result.status != call_state.status {
                return Err(DomainError::InvariantViolation(
                    "terminal tool call result does not match call state".into(),
                ));
            }
            if !tool_result_context_item_exists(state, run_id, turn_id, &result) {
                return Err(DomainError::InvariantViolation(
                    "tool batch cannot complete before result context items are recorded".into(),
                ));
            }
            results.push(result);
        }
        results
    };

    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;
    active_run.tool_batches.remove(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    active_run.completed_tool_batches.insert(
        batch_id,
        CompletedToolBatch {
            batch_id,
            run_id,
            turn_id,
            results,
        },
    );
    active_run.active_tool_batch_id = None;
    Ok(())
}

fn start_tool_call(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    batch_id: ToolBatchId,
    call_id: &ToolCallId,
    tool_name: &ToolName,
    arguments_ref: &BlobRef,
    execution_target: Option<&ToolExecutionTarget>,
) -> Result<(), DomainError> {
    let tool = state.tooling.registry.tools.get(tool_name).ok_or_else(|| {
        DomainError::InvariantViolation(format!(
            "tool call start references missing tool {}",
            tool_name
        ))
    })?;
    if !tool.invokes_client_effect() {
        return Err(DomainError::InvariantViolation(
            "tool call start requires a client-effect tool".into(),
        ));
    }
    validate_tool_execution_target_for_requirement(&tool.target_requirement, execution_target)?;

    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;
    if active_run.status != RunStatus::Active {
        return Err(DomainError::InvariantViolation(
            "tool calls can only start for active runs".into(),
        ));
    }
    if active_run.active_turn_id.is_some() {
        return Err(DomainError::InvariantViolation(
            "tool calls cannot start while a turn is active".into(),
        ));
    }
    if active_run.active_tool_batch_id != Some(batch_id) {
        return Err(DomainError::InvariantViolation(
            "tool call start does not match active tool batch".into(),
        ));
    }
    let batch = active_run.tool_batches.get_mut(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    if batch.run_id != run_id || batch.turn_id != turn_id {
        return Err(DomainError::InvariantViolation(
            "tool call start does not match tool batch run/turn".into(),
        ));
    }
    let call_state = batch
        .calls
        .iter_mut()
        .find(|call_state| call_state.call.call_id == *call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "tool call start references missing call {}",
                call_id
            ))
        })?;
    if call_state.status != ToolCallStatus::Accepted {
        return Err(DomainError::InvariantViolation(
            "tool call can only start from accepted state".into(),
        ));
    }
    if call_state.result.is_some() {
        return Err(DomainError::InvariantViolation(
            "tool call already has a result".into(),
        ));
    }
    if call_state.call.tool_name != *tool_name || call_state.call.arguments_ref != *arguments_ref {
        return Err(DomainError::InvariantViolation(
            "tool call start does not match observed tool call".into(),
        ));
    }
    call_state.status = ToolCallStatus::Pending;
    call_state.execution_target = execution_target.cloned();
    Ok(())
}

fn complete_tool_call(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    batch_id: ToolBatchId,
    result: &ToolCallResult,
) -> Result<(), DomainError> {
    if !matches!(
        result.status,
        ToolCallStatus::Succeeded | ToolCallStatus::Failed | ToolCallStatus::Cancelled
    ) {
        return Err(DomainError::InvariantViolation(
            "tool call completion must have a terminal call status".into(),
        ));
    }
    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;
    if active_run.active_tool_batch_id != Some(batch_id) {
        return Err(DomainError::InvariantViolation(
            "tool call completion does not match active tool batch".into(),
        ));
    }
    let batch = active_run.tool_batches.get_mut(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    if batch.run_id != run_id || batch.turn_id != turn_id {
        return Err(DomainError::InvariantViolation(
            "tool call completion does not match tool batch run/turn".into(),
        ));
    }
    let call_state = batch
        .calls
        .iter_mut()
        .find(|call_state| call_state.call.call_id == result.call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "tool call completion references missing call {}",
                result.call_id
            ))
        })?;
    if call_state.status != ToolCallStatus::Pending {
        return Err(DomainError::InvariantViolation(
            "tool call completion requires a pending tool call".into(),
        ));
    }
    if call_state.result.is_some() {
        return Err(DomainError::InvariantViolation(
            "tool call already has a result".into(),
        ));
    }
    call_state.status = result.status;
    call_state.result = Some(result.clone());
    Ok(())
}
