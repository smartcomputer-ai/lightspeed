use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ActiveRun, BlobRef, ContextEntry, ContextEntryInput, ContextEntryKind, ContextEntrySource,
    ContextEvent, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins, CoreAgentState,
    CoreAgentStatus, DomainError, ParkedToolBatch, PlanningError, PromiseOwnership,
    ProviderApiKind, RunId, RunStatus, ToolBatchId, ToolBatchSuspension, ToolCallId, ToolEffect,
    ToolName, TurnId, TurnOutcome, TurnStatus,
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
    },
    CallCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        result: ToolCallResult,
    },
    BatchDeferred {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        suspension: ToolBatchSuspension,
    },
    BatchResumed {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
    },
    BatchCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
    },
}

pub type ToolConfigEvent = ConfigEvent;
pub type ToolEvent = Event;

pub fn plan_next(state: &CoreAgentState) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    if state.lifecycle.status != CoreAgentStatus::Open {
        return Ok(Vec::new());
    }

    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(Vec::new());
    };
    if active_run.active_turn_id.is_some() {
        return Ok(Vec::new());
    }

    if let Some(batch_id) = active_run.active_tool_batch_id {
        return match active_run.status {
            RunStatus::Active => {
                let proposals = decide_active_tool_batch_invocations(state, active_run, batch_id)?;
                if proposals.is_empty() {
                    decide_active_tool_batch_completion(state, active_run, batch_id)
                } else {
                    Ok(proposals)
                }
            }
            RunStatus::Parked => Ok(Vec::new()),
            // A cancelling run resolves its own batch: every call
            // that is not terminal yet is recorded as cancelled with the
            // well-known cancelled-result content, then the batch completes
            // so the run component can reach `cancelled`. No new
            // invocations start and the runtime is never asked for work;
            // any in-flight call it still runs is abandoned.
            RunStatus::Cancelling => {
                let proposals = decide_cancelling_tool_batch_calls(active_run, batch_id)?;
                if proposals.is_empty() {
                    decide_active_tool_batch_completion(state, active_run, batch_id)
                } else {
                    Ok(proposals)
                }
            }
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled => Ok(Vec::new()),
        };
    }
    // Active runs start the batch for a completed tool-call turn; so do
    // cancelling runs, whose batch then resolves to cancelled results above,
    // so a cancelled run never leaves tool calls without results in context.
    if !matches!(active_run.status, RunStatus::Active | RunStatus::Cancelling) {
        return Ok(Vec::new());
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
            CoreAgentEvent::Tool(Event::BatchStarted {
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolingState {
    pub revision: u64,
    pub tools: BTreeMap<ToolName, ToolSpec>,
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
    validate_unique_native_mcp_prefixes(tools)?;

    for tool in tools.values() {
        tool.validate()?;
    }

    Ok(())
}

fn validate_unique_native_mcp_prefixes(
    tools: &BTreeMap<ToolName, ToolSpec>,
) -> Result<(), DomainError> {
    let prefixes = tools
        .values()
        .filter_map(|tool| match &tool.kind {
            ToolKind::RemoteMcp(spec)
                if spec.execution == RemoteMcpExecution::Native
                    && spec.exposure == RemoteMcpExposure::Inject =>
            {
                Some((format!("{}__", tool.name), &tool.name))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, (prefix, name)) in prefixes.iter().enumerate() {
        for (other_prefix, other_name) in prefixes.iter().skip(index + 1) {
            if prefix.starts_with(other_prefix) || other_prefix.starts_with(prefix) {
                return Err(DomainError::InvariantViolation(format!(
                    "native MCP tool prefixes for {name} and {other_name} are ambiguous"
                )));
            }
        }
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
    /// Admitted identity. Built-ins use logical ids; their exposed names are
    /// chosen by the runtime for each model request.
    pub name: ToolName,
    pub kind: ToolKind,
    pub parallelism: ToolParallelism,
    /// Runtime-owned execution policy facts admitted with the toolset. The
    /// hosted substrate selects activity deadlines and retry bounds from this
    /// admitted binding, never from model-controlled input.
    #[serde(default)]
    pub execution: ToolExecutionSpec,
}

/// Execution class for scheduling one tool call at the runtime boundary.
///
/// The class follows the logical tool domain, not the transport: environment
/// filesystem calls against a remote host share `Interactive` with local
/// calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionClass {
    /// Bounded interactive operation (filesystem, control, concurrency).
    #[default]
    Interactive,
    /// Bounded network-shaped call (web, environment jobs).
    RemoteInteractive,
    /// Environment process execution; its deadline derives from the validated
    /// process timeout clamped to the deployment-owned ceiling.
    Process,
    /// Bounded bulk data movement; progress and cancellation stay outside engine state.
    Bulk,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionSpec {
    #[serde(default)]
    pub class: ToolExecutionClass,
    /// Whether re-dispatching the call after an infrastructure failure is
    /// safe (read-only operations). Mutations and process starts stay at one
    /// attempt until downstream idempotency exists.
    #[serde(default)]
    pub retry_safe: bool,
}

impl ToolExecutionSpec {
    pub fn new(class: ToolExecutionClass, retry_safe: bool) -> Self {
        Self { class, retry_safe }
    }
}

impl ToolSpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        match &self.kind {
            ToolKind::Builtin(_) | ToolKind::Function(_) | ToolKind::ProviderNative(_) => Ok(()),
            ToolKind::RemoteMcp(remote_mcp) => remote_mcp.validate(),
        }
    }

    pub fn invokes_client_effect(&self) -> bool {
        match &self.kind {
            ToolKind::Builtin(_) | ToolKind::Function(_) => true,
            ToolKind::ProviderNative(native) => {
                native.execution == ProviderNativeToolExecution::ClientEffect
            }
            ToolKind::RemoteMcp(remote) => remote.execution == RemoteMcpExecution::Native,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Code-owned definition resolved by the runtime for the turn's model.
    Builtin(BuiltinToolSpec),
    Function(FunctionToolSpec),
    ProviderNative(ProviderNativeToolSpec),
    RemoteMcp(RemoteMcpToolSpec),
}

/// Small admitted settings for a code-owned tool. The tool's registry key is
/// its logical identity; the runtime owns settings validation and presentation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinToolSpec {
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionToolSpec {
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
    pub server_id: String,
    pub record_revision: u64,
    pub server_label: String,
    pub server_url: String,
    pub description_ref: Option<BlobRef>,
    pub allowed_tools: Option<Vec<String>>,
    pub execution: RemoteMcpExecution,
    pub exposure: RemoteMcpExposure,
    pub approval: RemoteMcpApprovalPolicy,
    pub defer_loading: Option<bool>,
    pub auth_ref: Option<SecretRef>,
    pub auth_required: bool,
    pub allow_private_network: bool,
}

impl RemoteMcpToolSpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_secret_ref_component("remote MCP server id", &self.server_id)?;
        if self.record_revision == 0 {
            return Err(DomainError::InvariantViolation(
                "remote MCP record revision must be >= 1".to_owned(),
            ));
        }
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
            if auth_ref.namespace != "mcp_server" || auth_ref.id != self.server_id {
                return Err(DomainError::InvariantViolation(
                    "remote MCP auth refs must identify the configured mcp_server".to_owned(),
                ));
            }
        } else if self.auth_required {
            return Err(DomainError::InvariantViolation(
                "remote MCP required auth needs a server credential ref".to_owned(),
            ));
        }
        if self.execution == RemoteMcpExecution::Provider
            && self.exposure != RemoteMcpExposure::Inject
        {
            return Err(DomainError::InvariantViolation(
                "remote MCP exposure applies only to native execution".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMcpExecution {
    #[default]
    Provider,
    Native,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMcpExposure {
    #[default]
    Inject,
    Search,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMcpApprovalPolicy {
    Never,
    Always,
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
    if value.is_empty() || value.len() > REMOTE_MCP_ALLOWED_TOOL_MAX_LEN {
        return Err(DomainError::InvariantViolation(
            "remote MCP server label must contain 1 to 128 bytes".to_owned(),
        ));
    }
    if !value
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric())
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(DomainError::InvariantViolation(
            "remote MCP server label must start with an ASCII letter or digit and contain only ASCII letters, digits, '_' or '-'".to_owned(),
        ));
    }
    Ok(())
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

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
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

/// Which tool the model must (or must not) call. Parallel tool-call
/// behavior is a separate generation knob (`parallel_tool_use`), not part of
/// the choice — bundling them was Anthropic request shape, not neutral
/// vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
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
    /// Admitted registry identity resolved by the response adapter. Absent for
    /// an unadvertised name. The original name remains transcript data below.
    pub tool_id: Option<ToolName>,
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
    /// First promise id this batch's executors may mint: one past the
    /// session cursor when the batch was created. Every dispatch of the
    /// batch (including per-call re-dispatch) numbers from here, so results
    /// applied in any order stay above every earlier batch's promises.
    #[serde(default)]
    pub promise_id_base: u64,
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
    pub result: Option<ToolCallResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallExecutionPolicy {
    pub invokes_client_effect: bool,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_visible_context_entries: Vec<ContextEntryInput>,
    pub error_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffect>,
    /// Immutable assets supplied by this result, separate from model input and effects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::Attachment>,
    /// Wall-clock milliseconds the executing runtime spent on this call,
    /// measured around the execution activity. Absent for synthetic results
    /// (cancelled, unavailable) and executors that do not measure. Recorded
    /// telemetry only — planning never branches on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Bytes of model-visible text the tool produced before the runtime's
    /// projection budget was applied. Absent for synthetic results and
    /// executors that do not measure. Recorded telemetry only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// True when the projection cut the model-visible text to its budget.
    /// Recorded telemetry only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

pub(crate) fn tool_result_context_item_exists(
    state: &CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    result: &ToolCallResult,
    provider_kind: Option<&str>,
) -> bool {
    tool_result_context_inputs(result, provider_kind).is_ok_and(|inputs| {
        inputs
            .iter()
            .all(|input| tool_result_context_input_exists(state, run_id, turn_id, result, input))
    })
}

fn tool_result_context_input_exists(
    state: &CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    result: &ToolCallResult,
    input: &ContextEntryInput,
) -> bool {
    state.context.entries.iter().any(|entry| {
        if !matches!(
            &entry.source,
            ContextEntrySource::Tool {
                run_id: item_run_id,
                turn_id: item_turn_id,
                ..
            } if *item_run_id == run_id && *item_turn_id == turn_id
        ) {
            return false;
        }
        if entry.kind != input.kind
            || entry.content != input.content
            || entry.preview != input.preview
            || entry.provenance_ref != input.provenance_ref
            || entry.token_estimate != input.token_estimate
        {
            return false;
        }
        match (&entry.kind, &input.kind) {
            (
                ContextEntryKind::ToolResult {
                    call_id: item_call_id,
                    is_error,
                },
                ContextEntryKind::ToolResult { .. },
            ) => item_call_id == &result.call_id && *is_error == result.status.is_error(),
            _ => true,
        }
    })
}

fn tool_result_context_inputs(
    result: &ToolCallResult,
    provider_kind: Option<&str>,
) -> Result<Vec<ContextEntryInput>, DomainError> {
    let mut inputs = result.model_visible_context_entries.clone();
    if inputs.is_empty() {
        return Err(DomainError::InvariantViolation(
            "terminal tool result is missing model-visible context entries".to_owned(),
        ));
    }
    validate_model_visible_tool_entries(result, &inputs)?;
    if !inputs
        .iter()
        .any(|entry| matches!(entry.kind, ContextEntryKind::ToolResult { .. }))
    {
        return Err(DomainError::InvariantViolation(
            "terminal tool result is missing a tool-result context entry".to_owned(),
        ));
    }
    for input in &mut inputs {
        if matches!(input.kind, ContextEntryKind::ToolResult { .. })
            && input.content.provider_kind.is_none()
        {
            input.content.provider_kind = provider_kind.map(ToOwned::to_owned);
        }
    }
    Ok(inputs)
}

fn validate_model_visible_tool_entries(
    result: &ToolCallResult,
    entries: &[ContextEntryInput],
) -> Result<(), DomainError> {
    for entry in entries {
        match &entry.kind {
            ContextEntryKind::ToolResult { call_id, is_error } => {
                if call_id != &result.call_id || *is_error != result.status.is_error() {
                    return Err(DomainError::InvariantViolation(
                        "tool-result context entry does not match terminal tool result".to_owned(),
                    ));
                }
            }
            ContextEntryKind::Message {
                role: crate::ContextMessageRole::User,
            }
            | ContextEntryKind::ProviderOpaque => {}
            _ => {
                return Err(DomainError::InvariantViolation(format!(
                    "terminal tool result cannot add model-visible context entry kind {:?}",
                    entry.kind
                )));
            }
        }
    }
    Ok(())
}

fn decide_active_tool_batch_invocations(
    _state: &CoreAgentState,
    active_run: &ActiveRun,
    batch_id: ToolBatchId,
) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active tool batch {} is missing", batch_id))
    })?;
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Ok(Vec::new());
    }
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
        let joins = CoreAgentJoins {
            run_id: Some(batch.run_id),
            turn_id: Some(batch.turn_id),
            tool_batch_id: Some(batch.batch_id),
            tool_call_id: Some(call_state.call.call_id.clone()),
            ..CoreAgentJoins::default()
        };
        proposals.push(CoreAgentEventProposal::new(
            joins,
            CoreAgentEvent::Tool(Event::CallStarted {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
                batch_id: batch.batch_id,
                call_id: call_state.call.call_id.clone(),
                tool_name: call_state.call.tool_name.clone(),
                arguments_ref: call_state.call.arguments_ref.clone(),
            }),
        ));
    }

    Ok(proposals)
}

/// Cancelled completions for every non-terminal call of the active batch of
/// a cancelling run. Parked batches are resumed through the await path and
/// are not touched here.
fn decide_cancelling_tool_batch_calls(
    active_run: &ActiveRun,
    batch_id: ToolBatchId,
) -> Result<Vec<CoreAgentEventProposal>, PlanningError> {
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("active tool batch {} is missing", batch_id))
    })?;
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Ok(Vec::new());
    }
    let mut proposals = Vec::new();
    for call_state in &batch.calls {
        if call_state.status.is_terminal() {
            continue;
        }
        let joins = CoreAgentJoins {
            run_id: Some(batch.run_id),
            turn_id: Some(batch.turn_id),
            tool_batch_id: Some(batch.batch_id),
            tool_call_id: Some(call_state.call.call_id.clone()),
            ..CoreAgentJoins::default()
        };
        proposals.push(CoreAgentEventProposal::new(
            joins,
            CoreAgentEvent::Tool(Event::CallCompleted {
                run_id: batch.run_id,
                turn_id: batch.turn_id,
                batch_id: batch.batch_id,
                result: cancelled_tool_result(&call_state.call),
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
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Ok(Vec::new());
    }
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
            CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision: state.context.revision,
                entries: result_items,
            }),
        ));
    }
    proposals.push(CoreAgentEventProposal::new(
        joins,
        CoreAgentEvent::Tool(Event::BatchCompleted {
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
        for input in tool_result_context_inputs(result, call_state.call.provider_kind.as_deref())? {
            if tool_result_context_input_exists(state, batch.run_id, batch.turn_id, result, &input)
            {
                continue;
            }
            inputs.push((
                None,
                ContextEntrySource::Tool {
                    run_id: batch.run_id,
                    turn_id: batch.turn_id,
                    batch_id: Some(batch.batch_id),
                },
                input,
            ));
        }
    }
    context_entries_from_inputs(state, inputs).map_err(Into::into)
}

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    let next_promise_id_base = state.id_cursors.last_promise_id + 1;
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
                if !matches!(active_run.status, RunStatus::Active | RunStatus::Cancelling) {
                    return Err(DomainError::InvariantViolation(
                        "tool batches can only start for active or cancelling runs".into(),
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
                        promise_id_base: next_promise_id_base,
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
        } => start_tool_call(
            state,
            *run_id,
            *turn_id,
            *batch_id,
            call_id,
            tool_name,
            arguments_ref,
        ),
        Event::CallCompleted {
            run_id,
            turn_id,
            batch_id,
            result,
        } => complete_tool_call(state, *run_id, *turn_id, *batch_id, result),
        Event::BatchDeferred {
            run_id,
            turn_id,
            batch_id,
            suspension,
        } => defer_tool_batch(state, *run_id, *turn_id, *batch_id, suspension.clone()),
        Event::BatchResumed {
            run_id,
            turn_id,
            batch_id,
        } => resume_deferred_tool_batch(state, *run_id, *turn_id, *batch_id),
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
        result,
    }
}

fn initial_tool_call_execution_policy(
    state: &CoreAgentState,
    turn_id: TurnId,
    call: &ObservedToolCall,
) -> Option<ToolCallExecutionPolicy> {
    let invokes_client_effect = call
        .tool_id
        .as_ref()
        .and_then(|tool_id| planned_tool_for_turn(state, turn_id, tool_id))
        .is_some_and(|tool| tool.invokes_client_effect());
    if !invokes_client_effect {
        return None;
    }
    Some(ToolCallExecutionPolicy {
        invokes_client_effect: true,
    })
}

pub fn remote_mcp_call_runtime(
    state: &CoreAgentState,
    call: &ObservedToolCall,
) -> Option<crate::RemoteMcpCallRuntime> {
    let tool_id = call.tool_id.as_ref()?;
    let call_id = &call.call_id;
    let approval_decision = state
        .runs
        .active
        .as_ref()
        .and_then(|run| {
            run.approvals.values().find_map(|record| {
                let crate::ApprovalContinuation::NativeMcp {
                    call_id: approval_call_id,
                } = &record.request.continuation
                else {
                    return None;
                };
                (approval_call_id == call_id).then_some(match record.status {
                    crate::ApprovalStatus::Approved => Some(true),
                    crate::ApprovalStatus::Rejected => Some(false),
                    crate::ApprovalStatus::Pending | crate::ApprovalStatus::Cancelled => None,
                })
            })
        })
        .flatten();

    if let Some((spec, remote_tool_name)) =
        resolve_injected_native_mcp(state, tool_id, &call.tool_name)
    {
        return Some(crate::RemoteMcpCallRuntime::Injected {
            target: remote_mcp_target(spec),
            remote_tool_name,
            approval_decision,
        });
    }
    if !matches!(tool_id.as_str(), "mcp.find_tools" | "mcp.call") {
        return None;
    }
    let targets = state
        .tooling
        .tools
        .values()
        .filter_map(|tool| match &tool.kind {
            ToolKind::RemoteMcp(spec)
                if spec.execution == RemoteMcpExecution::Native
                    && spec.exposure == RemoteMcpExposure::Search =>
            {
                Some(remote_mcp_target(spec))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    (!targets.is_empty()).then_some(crate::RemoteMcpCallRuntime::Search {
        targets,
        approval_decision,
    })
}

fn resolve_injected_native_mcp<'a>(
    state: &'a CoreAgentState,
    tool_id: &ToolName,
    exposed_name: &ToolName,
) -> Option<(&'a RemoteMcpToolSpec, String)> {
    let tool = state.tooling.tools.get(tool_id)?;
    let ToolKind::RemoteMcp(spec) = &tool.kind else {
        return None;
    };
    if spec.execution != RemoteMcpExecution::Native || spec.exposure != RemoteMcpExposure::Inject {
        return None;
    }
    let prefix = format!("{tool_id}__");
    let remote_name = exposed_name.as_str().strip_prefix(&prefix)?;
    if remote_name.is_empty()
        || spec
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.iter().any(|name| name == remote_name))
    {
        return None;
    }
    validate_remote_mcp_allowed_tool_name(remote_name).ok()?;
    Some((spec, remote_name.to_owned()))
}

fn remote_mcp_target(spec: &RemoteMcpToolSpec) -> crate::RemoteMcpCallTarget {
    crate::RemoteMcpCallTarget {
        server_id: spec.server_id.clone(),
        record_revision: spec.record_revision,
        server_label: spec.server_label.clone(),
        server_url: spec.server_url.clone(),
        allowed_tools: spec.allowed_tools.clone(),
        approval: spec.approval.clone(),
        auth_ref: spec.auth_ref.clone(),
        auth_required: spec.auth_required,
        allow_private_network: spec.allow_private_network,
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

/// Model-visible fallback content for tool calls that failed or were
/// cancelled at the runtime boundary when the specific error text could not
/// be materialized (e.g. the blob store itself is unavailable). Like
/// [`UNAVAILABLE_TOOL_RESULT_CONTENT`], every runtime must guarantee the
/// matching blob exists via [`crate::storage::ensure_engine_blobs`].
pub const TOOL_RUNTIME_BOUNDARY_FAILURE_CONTENT: &str =
    "tool call failed at the runtime boundary before its result could be recorded\n";

pub fn tool_runtime_boundary_failure_ref() -> BlobRef {
    BlobRef::from_bytes(TOOL_RUNTIME_BOUNDARY_FAILURE_CONTENT.as_bytes())
}

/// Model-visible content for tool calls the engine cancelled because their
/// run was cancelled before they completed. Like
/// [`UNAVAILABLE_TOOL_RESULT_CONTENT`], every runtime must guarantee the
/// matching blob exists via [`crate::storage::ensure_engine_blobs`].
pub const CANCELLED_TOOL_RESULT_CONTENT: &str =
    "tool call cancelled: the run was cancelled before this call completed\n";

pub fn cancelled_tool_result_ref() -> BlobRef {
    BlobRef::from_bytes(CANCELLED_TOOL_RESULT_CONTENT.as_bytes())
}

fn cancelled_tool_result(call: &ObservedToolCall) -> ToolCallResult {
    let error_ref = cancelled_tool_result_ref();
    let status = ToolCallStatus::Cancelled;
    ToolCallResult {
        attachments: Vec::new(),
        call_id: call.call_id.clone(),
        status,
        output_ref: None,
        model_visible_context_entries: vec![ContextEntryInput {
            kind: ContextEntryKind::ToolResult {
                call_id: call.call_id.clone(),
                is_error: status.is_error(),
            },
            content: crate::ContentRef {
                content_ref: error_ref.clone(),
                media_type: None,
                provider_kind: call.provider_kind.clone(),
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }],
        error_ref: Some(error_ref),
        effects: Vec::new(),
        duration_ms: None,
        output_bytes: None,
        truncated: false,
    }
}

fn unavailable_tool_result(call: &ObservedToolCall) -> ToolCallResult {
    let error_ref = unavailable_tool_result_ref();
    let status = ToolCallStatus::Unavailable;
    ToolCallResult {
        attachments: Vec::new(),
        call_id: call.call_id.clone(),
        status,
        output_ref: None,
        model_visible_context_entries: vec![ContextEntryInput {
            kind: ContextEntryKind::ToolResult {
                call_id: call.call_id.clone(),
                is_error: status.is_error(),
            },
            content: crate::ContentRef {
                content_ref: error_ref.clone(),
                media_type: None,
                provider_kind: call.provider_kind.clone(),
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }],
        error_ref: Some(error_ref),
        effects: Vec::new(),
        duration_ms: None,
        output_bytes: None,
        truncated: false,
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
        if active_run
            .parked_tool_batch
            .as_ref()
            .is_some_and(|parked| parked.batch_id == batch_id)
        {
            return Err(DomainError::InvariantViolation(
                "deferred tool batch cannot complete before it is resumed".into(),
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
            if !tool_result_context_item_exists(
                state,
                run_id,
                turn_id,
                &result,
                call_state.call.provider_kind.as_deref(),
            ) {
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

fn defer_tool_batch(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    batch_id: ToolBatchId,
    suspension: ToolBatchSuspension,
) -> Result<(), DomainError> {
    let active_run = crate::core::components::run::active_run_ref(state, run_id)?;
    if active_run.status != RunStatus::Active {
        return Err(DomainError::InvariantViolation(
            "tool batches can only defer for active runs".into(),
        ));
    }
    if active_run.active_turn_id.is_some() {
        return Err(DomainError::InvariantViolation(
            "tool batches cannot defer while a turn is active".into(),
        ));
    }
    if active_run.active_tool_batch_id != Some(batch_id) {
        return Err(DomainError::InvariantViolation(
            "deferred tool batch does not match active tool batch".into(),
        ));
    }
    if active_run.parked_tool_batch.is_some() {
        return Err(DomainError::InvariantViolation(format!(
            "tool batch {} is already deferred",
            batch_id
        )));
    }
    let batch = active_run.tool_batches.get(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    if batch.run_id != run_id || batch.turn_id != turn_id {
        return Err(DomainError::InvariantViolation(
            "deferred tool batch does not match run/turn".into(),
        ));
    }
    match &suspension {
        ToolBatchSuspension::AwaitTool { call_id, .. } => {
            if !batch.calls.iter().any(|call_state| {
                call_state.call.call_id == *call_id && call_state.status == ToolCallStatus::Pending
            }) {
                return Err(DomainError::InvariantViolation(format!(
                    "await call {} is not pending in tool batch {}",
                    call_id, batch_id
                )));
            }
        }
        ToolBatchSuspension::JoinedWorkflowCalls { calls, spec } => {
            if calls.is_empty() || spec.mode != crate::AwaitMode::All {
                return Err(DomainError::InvariantViolation(
                    "joined workflow suspension requires a non-empty all-of Promise wait".into(),
                ));
            }
            let mut call_ids = BTreeSet::new();
            let mut promise_ids = BTreeSet::new();
            for joined in calls {
                if !call_ids.insert(joined.call_id.clone())
                    || !promise_ids.insert(joined.promise_id.clone())
                {
                    return Err(DomainError::InvariantViolation(
                        "joined workflow suspension contains duplicate call or Promise identity"
                            .into(),
                    ));
                }
                if !batch.calls.iter().any(|call_state| {
                    call_state.call.call_id == joined.call_id
                        && call_state.status == ToolCallStatus::Pending
                }) {
                    return Err(DomainError::InvariantViolation(format!(
                        "joined workflow call {} is not pending in tool batch {}",
                        joined.call_id, batch_id
                    )));
                }
                let promise = state
                    .promises
                    .promises
                    .get(&joined.promise_id)
                    .ok_or_else(|| {
                        DomainError::InvariantViolation(format!(
                            "joined workflow call {} references missing Promise {}",
                            joined.call_id, joined.promise_id
                        ))
                    })?;
                if promise.ownership != PromiseOwnership::Runtime
                    || promise.scope != (crate::PromiseScope::Run { run_id })
                {
                    return Err(DomainError::InvariantViolation(format!(
                        "joined workflow Promise {} is not runtime-owned by run {}",
                        joined.promise_id, run_id
                    )));
                }
                match &promise.source {
                    crate::PromiseSource::Workflow {
                        invocation_id,
                        completion_key,
                        ..
                    } if invocation_id == joined.invocation_id.as_str()
                        && completion_key == crate::REPLY_COMPLETION_KEY => {}
                    _ => {
                        return Err(DomainError::InvariantViolation(format!(
                            "joined workflow Promise {} does not match invocation {} reply source",
                            joined.promise_id, joined.invocation_id
                        )));
                    }
                }
            }
            if spec.promise_ids
                != calls
                    .iter()
                    .map(|call| call.promise_id.clone())
                    .collect::<Vec<_>>()
            {
                return Err(DomainError::InvariantViolation(
                    "joined workflow suspension wait set does not match its call mappings".into(),
                ));
            }
        }
    }
    if batch.calls.iter().any(|call_state| {
        matches!(
            call_state.status,
            ToolCallStatus::Observed | ToolCallStatus::Accepted
        )
    }) {
        return Err(DomainError::InvariantViolation(
            "tool batch deferral requires all invocable calls to be pending".into(),
        ));
    }
    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;
    active_run.parked_tool_batch = Some(ParkedToolBatch {
        batch_id,
        suspension,
    });
    active_run.status = RunStatus::Parked;
    Ok(())
}

fn resume_deferred_tool_batch(
    state: &mut CoreAgentState,
    run_id: RunId,
    turn_id: TurnId,
    batch_id: ToolBatchId,
) -> Result<(), DomainError> {
    let active_run = crate::core::components::run::active_run_mut(state, run_id)?;
    if active_run.active_tool_batch_id != Some(batch_id) {
        return Err(DomainError::InvariantViolation(
            "resumed tool batch does not match active tool batch".into(),
        ));
    }
    let batch = active_run.tool_batches.get_mut(&batch_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("tool batch {} is missing", batch_id))
    })?;
    if batch.run_id != run_id || batch.turn_id != turn_id {
        return Err(DomainError::InvariantViolation(
            "resumed tool batch does not match run/turn".into(),
        ));
    }
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_none_or(|parked| parked.batch_id != batch_id)
    {
        return Err(DomainError::InvariantViolation(format!(
            "tool batch {} is not deferred",
            batch_id
        )));
    }
    active_run.parked_tool_batch = None;
    if active_run.status == RunStatus::Parked {
        active_run.status = RunStatus::Active;
    }
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
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Err(DomainError::InvariantViolation(
            "deferred tool batch cannot start calls".into(),
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
    if active_run
        .parked_tool_batch
        .as_ref()
        .is_some_and(|parked| parked.batch_id == batch_id)
    {
        return Err(DomainError::InvariantViolation(
            "deferred tool batch must be resumed before calls complete".into(),
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
    let cancellable = result.status == ToolCallStatus::Cancelled
        && matches!(
            call_state.status,
            ToolCallStatus::Observed | ToolCallStatus::Accepted | ToolCallStatus::Pending
        );
    if call_state.status != ToolCallStatus::Pending && !cancellable {
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
            server_id: server_label.to_owned(),
            record_revision: 1,
            server_label: server_label.to_owned(),
            server_url: server_url.to_owned(),
            description_ref: None,
            allowed_tools: Some(vec!["hello".to_owned()]),
            execution: RemoteMcpExecution::Provider,
            exposure: RemoteMcpExposure::Inject,
            approval: RemoteMcpApprovalPolicy::Never,
            defer_loading: Some(true),
            auth_ref: Some(SecretRef {
                namespace: "mcp_server".to_owned(),
                id: server_label.to_owned(),
            }),
            auth_required: true,
            allow_private_network: false,
        }
    }

    fn remote_mcp_tool(name: &str, server_label: &str) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            execution: Default::default(),
            kind: ToolKind::RemoteMcp(remote_mcp_spec(
                server_label,
                "https://echo.example.com/mcp",
            )),
            parallelism: ToolParallelism::ParallelSafe,
        }
    }

    #[test]
    fn remote_mcp_tool_is_not_a_client_effect() {
        let tool = remote_mcp_tool("mcp_echo", "echo");

        tool.validate().expect("valid remote MCP tool");
        assert!(!tool.invokes_client_effect());
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
            tool_id: Some(ToolName::new("missing_tool")),
            tool_name: ToolName::new("missing_tool"),
            provider_kind: None,
            arguments_ref: BlobRef::from_bytes(b"{}"),
            native_call_ref: None,
        };

        let result = unavailable_tool_result(&call);

        let expected = BlobRef::from_bytes(UNAVAILABLE_TOOL_RESULT_CONTENT.as_bytes());
        assert_eq!(
            result.model_visible_context_entries,
            vec![ContextEntryInput {
                kind: ContextEntryKind::ToolResult {
                    call_id: call.call_id.clone(),
                    is_error: true,
                },
                content: crate::ContentRef {
                    content_ref: expected.clone(),
                    media_type: None,
                    provider_kind: None
                },
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }]
        );
        assert_eq!(result.error_ref, Some(expected));
        assert_eq!(result.status, ToolCallStatus::Unavailable);
    }
}
