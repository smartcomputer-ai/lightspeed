use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActiveRun, BlobRef, ContextEntry, ContextEntryInput, ContextEntryKind, ContextEntrySource,
    ContextEvent, CoreAgentEventKind, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, PlanNext, PlanningError, ProviderApiKind, RunId, RunStatus,
    ToolBatchId, ToolCallId, ToolEffect, ToolName, TurnId, TurnOutcome, TurnStatus,
    core::components::context::context_entries_from_inputs,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigEvent {
    ToolsReplaced {
        base_revision: u64,
        tools: BTreeMap<ToolName, ToolSpec>,
    },
    ToolsPatched {
        base_revision: u64,
        patch: ToolPatch,
    },
    DefaultTargetSet {
        target: ToolExecutionTarget,
    },
    DefaultTargetCleared {
        namespace: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    BatchStarted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        toolset_revision: u64,
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
            let Some(planned) = turn.planned_request.as_ref() else {
                return Err(DomainError::InvariantViolation(format!(
                    "tool-call turn {} is missing planned request metadata",
                    turn_id
                ))
                .into());
            };
            if planned.toolset_revision != state.tooling.revision {
                return Err(DomainError::InvariantViolation(format!(
                    "planned toolset revision {} does not match active revision {}",
                    planned.toolset_revision, state.tooling.revision
                ))
                .into());
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
                    toolset_revision: planned.toolset_revision,
                    calls: facts.tool_calls.clone(),
                }),
            )]);
        }

        Ok(Vec::new())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolingState {
    pub revision: u64,
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub routing: ToolRoutingState,
}

pub fn validate_tool_map(tools: &BTreeMap<ToolName, ToolSpec>) -> Result<(), DomainError> {
    for (tool_name, tool) in tools {
        if &tool.name != tool_name {
            return Err(DomainError::InvariantViolation(format!(
                "tool map key {} does not match tool name {}",
                tool_name, tool.name
            )));
        }
    }

    validate_unique_remote_mcp_server_labels(tools)?;

    for tool in tools.values() {
        tool.validate()?;
    }

    Ok(())
}

fn validate_unique_remote_mcp_server_labels(
    tools: &BTreeMap<ToolName, ToolSpec>,
) -> Result<(), DomainError> {
    let mut labels = BTreeMap::<&str, &ToolName>::new();
    for (tool_name, tool) in tools {
        let ToolKind::RemoteMcp(remote_mcp) = &tool.kind else {
            continue;
        };
        if let Some(existing_tool_name) = labels.insert(remote_mcp.server_label.as_str(), tool_name)
        {
            return Err(DomainError::InvariantViolation(format!(
                "active tool set has duplicate remote MCP server label {} for tools {} and {}",
                remote_mcp.server_label, existing_tool_name, tool_name
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPatch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upsert: Vec<ToolSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<ToolName>,
}

impl ToolPatch {
    pub fn is_empty(&self) -> bool {
        self.upsert.is_empty() && self.remove.is_empty()
    }

    pub fn validate_for(&self, tools: &BTreeMap<ToolName, ToolSpec>) -> Result<(), DomainError> {
        let mut upsert_names = BTreeSet::new();
        for tool in &self.upsert {
            tool.validate()?;
            if !upsert_names.insert(tool.name.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "tool patch contains duplicate upsert {}",
                    tool.name
                )));
            }
        }

        let mut remove_names = BTreeSet::new();
        for tool_name in &self.remove {
            if !remove_names.insert(tool_name.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "tool patch contains duplicate remove {}",
                    tool_name
                )));
            }
            if upsert_names.contains(tool_name) {
                return Err(DomainError::InvariantViolation(format!(
                    "tool patch cannot both upsert and remove {}",
                    tool_name
                )));
            }
            if !tools.contains_key(tool_name) {
                return Err(DomainError::InvariantViolation(format!(
                    "tool patch removes missing tool {}",
                    tool_name
                )));
            }
        }

        Ok(())
    }

    pub fn apply_to(
        &self,
        tools: &BTreeMap<ToolName, ToolSpec>,
    ) -> Result<BTreeMap<ToolName, ToolSpec>, DomainError> {
        self.validate_for(tools)?;
        let mut next = tools.clone();
        for tool_name in &self.remove {
            next.remove(tool_name);
        }
        for tool in &self.upsert {
            next.insert(tool.name.clone(), tool.clone());
        }
        validate_tool_map(&next)?;
        Ok(next)
    }
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
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target_requirement.validate()?;
        match &self.kind {
            ToolKind::Function(_) | ToolKind::ProviderNative(_) => Ok(()),
            ToolKind::RemoteMcp(remote_mcp) => {
                if self.target_requirement != ToolTargetRequirement::None {
                    return Err(DomainError::InvariantViolation(format!(
                        "remote MCP tool {} must not declare an execution target requirement",
                        self.name
                    )));
                }
                remote_mcp.validate()
            }
        }
    }

    pub fn invokes_client_effect(&self) -> bool {
        match &self.kind {
            ToolKind::Function(_) => true,
            ToolKind::ProviderNative(native) => {
                native.execution == ProviderNativeToolExecution::ClientEffect
            }
            ToolKind::RemoteMcp(_) => false,
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
    RemoteMcp(RemoteMcpToolSpec),
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
pub struct RemoteMcpToolSpec {
    pub server_label: String,
    pub server_url: String,
    pub description_ref: Option<BlobRef>,
    pub allowed_tools: Option<Vec<String>>,
    pub approval: RemoteMcpApprovalPolicy,
    pub defer_loading: Option<bool>,
    pub auth_ref: Option<SecretRef>,
}

impl RemoteMcpToolSpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_remote_mcp_server_label(&self.server_label)?;
        validate_remote_mcp_server_url(&self.server_url)?;
        if let Some(allowed_tools) = &self.allowed_tools {
            if allowed_tools.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "remote MCP allowed_tools must not be empty when present".to_owned(),
                ));
            }
            let mut seen = BTreeSet::new();
            for tool_name in allowed_tools {
                validate_remote_mcp_allowed_tool_name(tool_name)?;
                if !seen.insert(tool_name.as_str()) {
                    return Err(DomainError::InvariantViolation(format!(
                        "remote MCP allowed_tools contains duplicate tool name {}",
                        tool_name
                    )));
                }
            }
        }
        if let Some(auth_ref) = &self.auth_ref {
            auth_ref.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMcpApprovalPolicy {
    ProviderDefault,
    Always,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub namespace: String,
    pub id: String,
}

impl SecretRef {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_secret_ref_component("secret ref namespace", &self.namespace)?;
        validate_secret_ref_component("secret ref id", &self.id)
    }
}

const REMOTE_MCP_URL_MAX_LEN: usize = 2048;
const REMOTE_MCP_ALLOWED_TOOL_MAX_LEN: usize = 128;

fn validate_remote_mcp_server_label(value: &str) -> Result<(), DomainError> {
    validate_target_component(
        "remote MCP server label",
        value,
        "ASCII letters, digits, '_', '-'",
        |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'),
    )
}

fn validate_remote_mcp_server_url(value: &str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::InvariantViolation(
            "remote MCP server URL must not be empty".to_owned(),
        ));
    }
    if value.len() > REMOTE_MCP_URL_MAX_LEN {
        return Err(DomainError::InvariantViolation(format!(
            "remote MCP server URL is too long: {} bytes, max {}",
            value.len(),
            REMOTE_MCP_URL_MAX_LEN
        )));
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(|ch| ch.is_control()) {
        return Err(DomainError::InvariantViolation(
            "remote MCP server URL must not contain whitespace or control characters".to_owned(),
        ));
    }
    if value.contains('#') {
        return Err(DomainError::InvariantViolation(
            "remote MCP server URL must not contain a fragment".to_owned(),
        ));
    }

    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(DomainError::InvariantViolation(
            "remote MCP server URL must include http:// or https:// scheme".to_owned(),
        ));
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(DomainError::InvariantViolation(format!(
            "remote MCP server URL scheme {scheme:?} is not supported"
        )));
    }

    let authority_end = rest
        .find(|ch| matches!(ch, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(DomainError::InvariantViolation(
            "remote MCP server URL host must not be empty".to_owned(),
        ));
    }
    if authority.contains('@') {
        return Err(DomainError::InvariantViolation(
            "remote MCP server URL must not include credentials".to_owned(),
        ));
    }

    if let Some(stripped) = authority.strip_prefix('[') {
        let Some(end) = stripped.find(']') else {
            return Err(DomainError::InvariantViolation(
                "remote MCP server URL IPv6 host is missing closing ']'".to_owned(),
            ));
        };
        if end == 0 {
            return Err(DomainError::InvariantViolation(
                "remote MCP server URL host must not be empty".to_owned(),
            ));
        }
    } else {
        let host = authority.split(':').next().unwrap_or(authority);
        if host.is_empty() {
            return Err(DomainError::InvariantViolation(
                "remote MCP server URL host must not be empty".to_owned(),
            ));
        }
    }

    Ok(())
}

fn validate_remote_mcp_allowed_tool_name(value: &str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::InvariantViolation(
            "remote MCP allowed tool name must not be empty".to_owned(),
        ));
    }
    if value.len() > REMOTE_MCP_ALLOWED_TOOL_MAX_LEN {
        return Err(DomainError::InvariantViolation(format!(
            "remote MCP allowed tool name is too long: {} bytes, max {}",
            value.len(),
            REMOTE_MCP_ALLOWED_TOOL_MAX_LEN
        )));
    }
    if value.trim() != value || value.chars().any(char::is_whitespace) {
        return Err(DomainError::InvariantViolation(
            "remote MCP allowed tool name must not contain whitespace".to_owned(),
        ));
    }
    if value.chars().any(|ch| ch.is_control()) {
        return Err(DomainError::InvariantViolation(
            "remote MCP allowed tool name must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

fn validate_secret_ref_component(kind: &'static str, value: &str) -> Result<(), DomainError> {
    crate::validate_general_string_id(kind, value)
        .map_err(|error| DomainError::InvariantViolation(error.to_string()))
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
    pub execution_policy: Option<ToolCallExecutionPolicy>,
    pub execution_target: Option<ToolExecutionTarget>,
    pub result: Option<ToolCallResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallExecutionPolicy {
    pub invokes_client_effect: bool,
    pub target_requirement: ToolTargetRequirement,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffect>,
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
    state.context.entries.iter().any(|entry| {
        matches!(
            (&entry.source, &entry.kind),
            (
                ContextEntrySource::Tool {
                    run_id: item_run_id,
                    turn_id: item_turn_id,
                    ..
                },
                ContextEntryKind::ToolResult {
                    call_id: item_call_id,
                    is_error,
                },
            ) if *item_run_id == run_id
                && *item_turn_id == turn_id
                && item_call_id == &result.call_id
                && *is_error == result.status.is_error()
                && entry.content_ref == *expected_ref
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
        let Some(policy) = call_state.execution_policy.as_ref() else {
            return Err(DomainError::InvariantViolation(format!(
                "accepted tool call {} is missing execution policy",
                call_state.call.call_id
            ))
            .into());
        };
        if !policy.invokes_client_effect {
            return Err(DomainError::InvariantViolation(format!(
                "accepted tool call {} does not invoke a client effect",
                call_state.call.call_id
            ))
            .into());
        }
        let execution_target =
            resolve_tool_execution_target(state, &call_state.call.tool_name, policy)?;

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
    let result_items = tool_result_context_entries(state, batch)?;
    let joins = CoreAgentJoins {
        run_id: Some(batch.run_id),
        turn_id: Some(batch.turn_id),
        tool_batch_id: Some(batch.batch_id),
        ..CoreAgentJoins::default()
    };
    if !result_items.is_empty() {
        proposals.push(CoreAgentEventProposal::new(
            joins.clone(),
            CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
                base_revision: state.context.revision,
                entries: result_items,
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

fn tool_result_context_entries(
    state: &CoreAgentState,
    batch: &ActiveToolBatch,
) -> Result<Vec<ContextEntry>, PlanningError> {
    let mut inputs = Vec::new();
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
        let content_ref = tool_result_context_ref(result).cloned().ok_or_else(|| {
            DomainError::InvariantViolation(
                "terminal tool result is missing a model-visible ref".to_owned(),
            )
        })?;
        inputs.push((
            None,
            ContextEntrySource::Tool {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
                batch_id: Some(batch.batch_id),
            },
            ContextEntryInput {
                kind: ContextEntryKind::ToolResult {
                    call_id: result.call_id.clone(),
                    is_error: result.status.is_error(),
                },
                content_ref,
                media_type: None,
                preview: None,
                provider_kind: call_state.call.provider_kind.clone(),
                provider_item_id: None,
                token_estimate: None,
            },
        ));
    }
    context_entries_from_inputs(state, inputs).map_err(Into::into)
}

fn resolve_tool_execution_target(
    state: &CoreAgentState,
    tool_name: &ToolName,
    policy: &ToolCallExecutionPolicy,
) -> Result<Option<ToolExecutionTarget>, PlanningError> {
    policy.target_requirement.validate()?;
    match &policy.target_requirement {
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
                    tool_name, namespace
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
            toolset_revision,
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
            let planned_toolset_revision =
                planned_toolset_revision_for_turn(state, *run_id, *turn_id)?;
            if *toolset_revision != planned_toolset_revision {
                return Err(DomainError::InvariantViolation(format!(
                    "tool batch toolset revision {} does not match planned revision {}",
                    toolset_revision, planned_toolset_revision
                )));
            }
            if *toolset_revision != state.tooling.revision {
                return Err(DomainError::InvariantViolation(format!(
                    "tool batch toolset revision {} does not match active revision {}",
                    toolset_revision, state.tooling.revision
                )));
            }
            let call_states = calls
                .iter()
                .map(|call| initial_tool_call_state(state, *turn_id, call))
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
        ConfigEvent::ToolsReplaced {
            base_revision,
            tools,
        } => {
            validate_tooling_base_revision(state, *base_revision)?;
            validate_tool_map(tools)?;
            state.tooling.tools = tools.clone();
            bump_tooling_revision(state)?;
            Ok(())
        }
        ConfigEvent::ToolsPatched {
            base_revision,
            patch,
        } => {
            validate_tooling_base_revision(state, *base_revision)?;
            state.tooling.tools = patch.apply_to(&state.tooling.tools)?;
            bump_tooling_revision(state)?;
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

fn validate_tooling_base_revision(
    state: &CoreAgentState,
    base_revision: u64,
) -> Result<(), DomainError> {
    if base_revision == state.tooling.revision {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "tool event base revision {} does not match active revision {}",
            base_revision, state.tooling.revision
        )))
    }
}

fn bump_tooling_revision(state: &mut CoreAgentState) -> Result<(), DomainError> {
    state.tooling.revision = state
        .tooling
        .revision
        .checked_add(1)
        .ok_or_else(|| DomainError::InvariantViolation("tool revision exhausted".to_owned()))?;
    Ok(())
}

fn initial_tool_call_state(
    state: &CoreAgentState,
    turn_id: TurnId,
    call: &ObservedToolCall,
) -> ToolCallState {
    let execution_policy = initial_tool_call_execution_policy(state, turn_id, call);
    let status = if execution_policy.is_some() {
        ToolCallStatus::Accepted
    } else {
        ToolCallStatus::Unavailable
    };
    let result = if status == ToolCallStatus::Unavailable {
        Some(unavailable_tool_result(call))
    } else {
        None
    };
    ToolCallState {
        call: call.clone(),
        status,
        execution_policy,
        execution_target: None,
        result,
    }
}

fn initial_tool_call_execution_policy(
    state: &CoreAgentState,
    turn_id: TurnId,
    call: &ObservedToolCall,
) -> Option<ToolCallExecutionPolicy> {
    let Some(tool) = planned_tool_for_turn(state, turn_id, &call.tool_name) else {
        return None;
    };
    if !tool.invokes_client_effect() {
        return None;
    }
    if tool.target_requirement.validate().is_err() {
        return None;
    }
    if let Some(namespace) = required_target_namespace(&tool.target_requirement) {
        if !state
            .tooling
            .routing
            .default_targets
            .contains_key(namespace)
        {
            return None;
        }
    }
    Some(ToolCallExecutionPolicy {
        invokes_client_effect: true,
        target_requirement: tool.target_requirement,
    })
}

fn required_target_namespace(requirement: &ToolTargetRequirement) -> Option<&str> {
    match requirement {
        ToolTargetRequirement::Required { namespace } => Some(namespace.as_str()),
        ToolTargetRequirement::None | ToolTargetRequirement::Optional { .. } => None,
    }
}

fn planned_tool_for_turn(
    state: &CoreAgentState,
    turn_id: TurnId,
    tool_name: &ToolName,
) -> Option<ToolSpec> {
    let active_run = state.runs.active.as_ref()?;
    let turn = active_run.turns.get(&turn_id)?;
    let planned = turn.planned_request.as_ref()?;
    if planned.toolset_revision != state.tooling.revision {
        return None;
    }
    state.tooling.tools.get(tool_name).cloned()
}

fn planned_toolset_revision_for_turn(
    state: &CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
) -> Result<u64, DomainError> {
    let active_run = crate::core::components::run::active_run_ref(state, run_id)?;
    let turn = active_run.turns.get(&turn_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch turn {} is missing", turn_id))
    })?;
    let planned = turn.planned_request.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation(format!(
            "tool batch turn {} is missing planned request metadata",
            turn_id
        ))
    })?;
    Ok(planned.toolset_revision)
}

/// Model-visible content for tool calls the engine marks unavailable.
///
/// The deterministic core cannot write blobs, so unavailable results reference
/// this well-known constant content by hash. Every runtime that fulfills core
/// actions must guarantee the matching blob exists (see
/// [`crate::storage::ensure_engine_blobs`]); content-addressed puts make that
/// idempotent.
pub const UNAVAILABLE_TOOL_RESULT_CONTENT: &str =
    "tool unavailable: this tool cannot be invoked in this session\n";

pub fn unavailable_tool_result_ref() -> BlobRef {
    BlobRef::from_bytes(UNAVAILABLE_TOOL_RESULT_CONTENT.as_bytes())
}

fn unavailable_tool_result(call: &ObservedToolCall) -> ToolCallResult {
    let error_ref = unavailable_tool_result_ref();
    ToolCallResult {
        call_id: call.call_id.clone(),
        status: ToolCallStatus::Unavailable,
        output_ref: None,
        model_visible_output_ref: Some(error_ref.clone()),
        error_ref: Some(error_ref),
        effects: Vec::new(),
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
    let policy = {
        let active_run = crate::core::components::run::active_run_ref(state, run_id)?;
        tool_call_execution_policy_for_start(
            active_run,
            turn_id,
            batch_id,
            call_id,
            tool_name,
            arguments_ref,
        )?
    };
    if !policy.invokes_client_effect {
        return Err(DomainError::InvariantViolation(
            "tool call start requires a client-effect tool".into(),
        ));
    }
    validate_tool_execution_target_for_requirement(&policy.target_requirement, execution_target)?;

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

fn tool_call_execution_policy_for_start(
    active_run: &ActiveRun,
    turn_id: TurnId,
    batch_id: ToolBatchId,
    call_id: &ToolCallId,
    tool_name: &ToolName,
    arguments_ref: &BlobRef,
) -> Result<ToolCallExecutionPolicy, DomainError> {
    if active_run.active_tool_batch_id != Some(batch_id) {
        return Err(DomainError::InvariantViolation(
            "tool call start does not match active tool batch".into(),
        ));
    }
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    if batch.turn_id != turn_id {
        return Err(DomainError::InvariantViolation(
            "tool call start does not match tool batch turn".into(),
        ));
    }
    let call_state = batch
        .calls
        .iter()
        .find(|call_state| call_state.call.call_id == *call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "tool call start references missing call {}",
                call_id
            ))
        })?;
    if call_state.call.tool_name != *tool_name || call_state.call.arguments_ref != *arguments_ref {
        return Err(DomainError::InvariantViolation(
            "tool call start does not match accepted call".into(),
        ));
    }
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
    call_state.execution_policy.clone().ok_or_else(|| {
        DomainError::InvariantViolation(format!(
            "accepted tool call {} is missing execution policy",
            call_id
        ))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_mcp_spec(server_label: &str, server_url: &str) -> RemoteMcpToolSpec {
        RemoteMcpToolSpec {
            server_label: server_label.to_owned(),
            server_url: server_url.to_owned(),
            description_ref: None,
            allowed_tools: Some(vec!["hello".to_owned()]),
            approval: RemoteMcpApprovalPolicy::Never,
            defer_loading: Some(true),
            auth_ref: Some(SecretRef {
                namespace: "mcp_grant".to_owned(),
                id: "mcpgrant_123".to_owned(),
            }),
        }
    }

    fn remote_mcp_tool(name: &str, server_label: &str) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            kind: ToolKind::RemoteMcp(remote_mcp_spec(
                server_label,
                "https://echo.example.com/mcp",
            )),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: ToolTargetRequirement::None,
        }
    }

    #[test]
    fn remote_mcp_tool_is_not_a_client_effect() {
        let tool = remote_mcp_tool("mcp_echo", "echo");

        tool.validate().expect("valid remote MCP tool");
        assert!(!tool.invokes_client_effect());
    }

    #[test]
    fn remote_mcp_tool_rejects_execution_target_requirement() {
        let mut tool = remote_mcp_tool("mcp_echo", "echo");
        tool.target_requirement = ToolTargetRequirement::required("host");

        let error = tool
            .validate()
            .expect_err("remote MCP must not declare an execution target");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn remote_mcp_validation_rejects_url_credentials() {
        let mut spec = remote_mcp_spec("echo", "https://echo.example.com/mcp");
        spec.server_url = "https://user:secret@echo.example.com/mcp".to_owned();

        let error = spec
            .validate()
            .expect_err("remote MCP URL credentials must be rejected");

        let DomainError::InvariantViolation(message) = error else {
            panic!("expected invariant violation, got {error:?}");
        };
        assert!(message.contains("credentials"));
    }

    #[test]
    fn remote_mcp_validation_rejects_duplicate_allowed_tools() {
        let mut spec = remote_mcp_spec("echo", "https://echo.example.com/mcp");
        spec.allowed_tools = Some(vec!["hello".to_owned(), "hello".to_owned()]);

        let error = spec
            .validate()
            .expect_err("duplicate allowed_tools entries must be rejected");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn tool_map_rejects_duplicate_remote_mcp_labels() {
        let first = remote_mcp_tool("mcp_echo_one", "echo");
        let second = remote_mcp_tool("mcp_echo_two", "echo");
        let first_name = first.name.clone();
        let second_name = second.name.clone();
        let tools = BTreeMap::from([(first_name.clone(), first), (second_name.clone(), second)]);

        let error = validate_tool_map(&tools)
            .expect_err("duplicate remote MCP labels in active tools must be rejected");

        let DomainError::InvariantViolation(message) = error else {
            panic!("expected invariant violation, got {error:?}");
        };
        assert!(message.contains("duplicate remote MCP server label echo"));
    }

    #[test]
    fn unavailable_tool_results_reference_the_well_known_constant_blob() {
        let call = ObservedToolCall {
            call_id: crate::ToolCallId::new("call_1"),
            tool_name: ToolName::new("missing_tool"),
            provider_kind: None,
            arguments_ref: BlobRef::from_bytes(b"{}"),
            native_call_ref: None,
        };

        let result = unavailable_tool_result(&call);

        let expected = BlobRef::from_bytes(UNAVAILABLE_TOOL_RESULT_CONTENT.as_bytes());
        assert_eq!(result.model_visible_output_ref, Some(expected.clone()));
        assert_eq!(result.error_ref, Some(expected));
        assert_eq!(result.status, ToolCallStatus::Unavailable);
    }
}
