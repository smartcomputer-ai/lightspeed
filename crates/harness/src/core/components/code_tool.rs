//! Durable calls made by a workflow-backed tool while its parent call is parked.
//! The parent batch supplies causal joins only; code tool results never become a
//! model turn or replace the parent batch's suspension.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AwaitMode, AwaitSpec, BlobRef, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins,
    CoreAgentState, DomainError, PromiseEvent, PromiseOwnership, PromiseStatus, RunStatus,
    SessionId, ToolBatchSuspension, ToolCallId, ToolCallStatus, ToolInvocationBatchRequest,
    ToolInvocationBatchResult, ToolInvocationResult, ToolName, WakeReason, WorkflowToolCompletion,
    WorkflowToolInvocation, WorkflowToolInvocationId,
};

pub const MAX_CODE_TOOL_SCOPES_PER_RUN: usize = 32;
pub const MAX_CODE_TOOL_CALLS_PER_SCOPE: u32 = 1024;
pub const MAX_CODE_TOOL_IN_FLIGHT: u32 = 64;
pub const CODE_TOOL_PROMISE_SLOTS: u64 = crate::MAX_COMPLETION_PROMISES as u64;

/// Force recovery cannot prove whether an abandoned activity committed an
/// external mutation. Its durable outcome is unknown, not a successful cancel.
pub const CODE_TOOL_INTERRUPTED_CONTENT: &str = "Code tool execution was interrupted when its owning run ended. Its outcome is unknown; an external side effect may already have occurred.";

pub fn code_tool_interrupted_ref() -> BlobRef {
    BlobRef::from_bytes(CODE_TOOL_INTERRUPTED_CONTENT.as_bytes())
}

/// Run termination is itself the durable interruption fact. Ordinary
/// cancellation drains calls before terminating; force recovery must also
/// terminate Update waiters without pretending unfinished effects were undone.
pub(crate) fn interrupt_code_tools_for_run(state: &mut CoreAgentState, run_id: crate::RunId) {
    let scopes = state
        .code_tools
        .scopes
        .iter()
        .filter(|(_, scope)| {
            code_tool_parent(state, &scope.spec).is_ok_and(|parent| parent.run_id == run_id)
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in scopes {
        let scope = state
            .code_tools
            .scopes
            .get_mut(&id)
            .expect("scope was collected above");
        scope.closed = true;
        scope.cancel_requested = true;
        for call in scope
            .calls
            .values_mut()
            .filter(|call| !call.status.is_terminal())
        {
            call.status = CodeToolCallStatus::Completed {
                result: ToolInvocationResult {
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Unavailable,
                    output_ref: None,
                    error_ref: Some(code_tool_interrupted_ref()),
                    model_visible_context_entries: Vec::new(),
                    effects: Vec::new(),
                    attachments: Vec::new(),
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                }
                .into(),
            };
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolBinding {
    pub tool_id: ToolName,
    pub tool_name: ToolName,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolScopeSpec {
    pub execution_id: String,
    pub parent_invocation_id: WorkflowToolInvocationId,
    pub bindings: BTreeMap<String, CodeToolBinding>,
    pub max_calls: u32,
    pub max_in_flight: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolOrigin {
    pub execution_id: String,
    pub request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolCallSpec {
    pub origin: CodeToolOrigin,
    pub binding_id: String,
    pub tool_id: ToolName,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolCall {
    pub spec: CodeToolCallSpec,
    pub call_id: ToolCallId,
    pub promise_id_base: u64,
    pub status: CodeToolCallStatus,
}

/// Durable code tool outcome. Effect carriers are admitted as their own domain
/// events, and model-context projections do not belong to a code tool caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CodeToolCallResultWire")]
pub struct CodeToolCallResult {
    pub call_id: ToolCallId,
    pub status: ToolCallStatus,
    pub output_ref: Option<BlobRef>,
    pub error_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    // Plain hexadecimal, not a BlobRef: the original activity result need not
    // be stored. Include every field so changed effects cannot pass as retries.
    invocation_result_digest: String,
}

impl CodeToolCallResult {
    fn matches_invocation_result(&self, result: &ToolInvocationResult) -> bool {
        self.invocation_result_digest == invocation_result_digest(result)
    }
}

impl From<ToolInvocationResult> for CodeToolCallResult {
    fn from(result: ToolInvocationResult) -> Self {
        let invocation_result_digest = invocation_result_digest(&result);
        Self {
            call_id: result.call_id,
            status: result.status,
            output_ref: result.output_ref,
            error_ref: result.error_ref,
            attachments: result.attachments,
            duration_ms: result.duration_ms,
            output_bytes: result.output_bytes,
            truncated: result.truncated,
            invocation_result_digest,
        }
    }
}

fn invocation_result_digest(result: &ToolInvocationResult) -> String {
    let bytes = serde_json::to_vec(result).expect("tool invocation results are serializable");
    hex::encode(Sha256::digest(bytes))
}

/// Read earlier code tool event/snapshot records before discarding their full
/// context and effects. Compact records already carry the original digest.
#[derive(Deserialize)]
struct CodeToolCallResultWire {
    #[serde(default)]
    invocation_result_digest: Option<String>,
    #[serde(flatten)]
    result: ToolInvocationResult,
}

impl TryFrom<CodeToolCallResultWire> for CodeToolCallResult {
    type Error = &'static str;

    fn try_from(wire: CodeToolCallResultWire) -> Result<Self, Self::Error> {
        let mut result = Self::from(wire.result);
        if let Some(digest) = wire.invocation_result_digest {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("invalid code tool invocation result digest");
            }
            result.invocation_result_digest = digest;
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodeToolCallStatus {
    Pending,
    Waiting { suspension: ToolBatchSuspension },
    Completed { result: CodeToolCallResult },
}

impl CodeToolCallStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolScope {
    pub toolset_revision: u64,
    pub spec: CodeToolScopeSpec,
    pub closed: bool,
    pub cancel_requested: bool,
    pub calls: BTreeMap<String, CodeToolCall>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolState {
    pub scopes: BTreeMap<String, CodeToolScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeToolEvent {
    ScopeOpened {
        scope: CodeToolScopeSpec,
    },
    CallAdmitted {
        call: CodeToolCallSpec,
        promise_id_base: u64,
    },
    CallCompleted {
        origin: CodeToolOrigin,
        result: CodeToolCallResult,
    },
    CallDeferred {
        origin: CodeToolOrigin,
        suspension: ToolBatchSuspension,
    },
    ScopeClosed {
        execution_id: String,
        cancel: bool,
    },
}

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::InvariantViolation(message.into())
}

fn validate_id(kind: &'static str, value: &str) -> Result<(), DomainError> {
    if value.len() > 256 {
        return Err(invalid(format!("code tool {kind} exceeds 256 bytes")));
    }
    crate::validate_general_string_id(kind, value).map_err(|error| invalid(error.to_string()))
}

pub fn code_tool_call_id(origin: &CodeToolOrigin) -> ToolCallId {
    let bytes = serde_json::to_vec(&(origin.execution_id.as_str(), origin.request_id.as_str()))
        .expect("string pair is serializable");
    ToolCallId::new(format!("code-tool:{}", hex::encode(Sha256::digest(bytes))))
}

pub fn code_tool_parent<'a>(
    state: &'a CoreAgentState,
    scope: &CodeToolScopeSpec,
) -> Result<&'a WorkflowToolInvocation, DomainError> {
    state
        .workflow_tools
        .emissions
        .get(&scope.parent_invocation_id)
        .or_else(|| {
            state
                .workflow_tools
                .start_requests
                .get(&scope.parent_invocation_id)
        })
        .ok_or_else(|| invalid("code tool scope references unknown parent workflow invocation"))
}

pub fn code_tool_call<'a>(
    state: &'a CoreAgentState,
    origin: &CodeToolOrigin,
) -> Option<&'a CodeToolCall> {
    state
        .code_tools
        .scopes
        .get(&origin.execution_id)?
        .calls
        .get(&origin.request_id)
}

pub(crate) fn code_tool_call_for_id<'a>(
    state: &'a CoreAgentState,
    call_id: &ToolCallId,
) -> Option<(&'a CodeToolScope, &'a CodeToolCall)> {
    state.code_tools.scopes.values().find_map(|scope| {
        scope
            .calls
            .values()
            .find(|call| &call.call_id == call_id)
            .map(|call| (scope, call))
    })
}

pub fn code_tool_scope_is_live(state: &CoreAgentState, scope: &CodeToolScopeSpec) -> bool {
    validate_live_parent(state, scope).is_ok()
}

fn validate_live_parent(
    state: &CoreAgentState,
    scope: &CodeToolScopeSpec,
) -> Result<(), DomainError> {
    let parent = code_tool_parent(state, scope)?;
    let run = state
        .runs
        .active
        .as_ref()
        .ok_or_else(|| invalid("code tool calls require an active run"))?;
    if run.run_id != parent.run_id || !matches!(run.status, RunStatus::Active | RunStatus::Parked) {
        return Err(invalid("code tool parent run is not accepting calls"));
    }
    let parked = run
        .parked_tool_batch
        .as_ref()
        .ok_or_else(|| invalid("code tool parent is not parked"))?;
    let ToolBatchSuspension::JoinedWorkflowCalls { calls, .. } = &parked.suspension else {
        return Err(invalid("code tool parent must be a joined workflow call"));
    };
    if parked.batch_id != parent.tool_batch_id
        || !calls.iter().any(|call| {
            call.invocation_id == parent.invocation_id
                && call.call_id == parent.tool_call_id
                && state
                    .promises
                    .promises
                    .get(&call.promise_id)
                    .is_some_and(|promise| promise.status == PromiseStatus::Pending)
        })
    {
        return Err(invalid("code tool parent joined call is no longer pending"));
    }
    Ok(())
}

fn validate_scope(state: &CoreAgentState, scope: &CodeToolScopeSpec) -> Result<(), DomainError> {
    validate_id("execution_id", &scope.execution_id)?;
    validate_live_parent(state, scope)?;
    if scope.max_calls == 0
        || scope.max_calls > MAX_CODE_TOOL_CALLS_PER_SCOPE
        || scope.max_in_flight == 0
        || scope.max_in_flight > MAX_CODE_TOOL_IN_FLIGHT
        || scope.max_in_flight > scope.max_calls
        || scope.bindings.len() > 4096
    {
        return Err(invalid(
            "code tool scope limits are outside their bounded ranges",
        ));
    }
    let parent = code_tool_parent(state, scope)?;
    let parent_tool = state
        .workflow_tools
        .bindings
        .get(&parent.tool_id)
        .ok_or_else(|| invalid("code tool parent binding is missing"))?;
    for (binding_id, binding) in &scope.bindings {
        validate_id("binding_id", binding_id)?;
        let tool_id = &binding.tool_id;
        let tool = state
            .tooling
            .tools
            .get(tool_id)
            .ok_or_else(|| invalid(format!("code tool {tool_id} is not granted")))?;
        if !tool.invokes_client_effect() || tool_id == &parent_tool.definition.tool.name {
            return Err(invalid(format!(
                "tool {tool_id} is not available for code tool execution"
            )));
        }
    }
    let count = state
        .code_tools
        .scopes
        .values()
        .filter(|existing| {
            code_tool_parent(state, &existing.spec)
                .is_ok_and(|existing| existing.run_id == parent.run_id)
        })
        .count();
    if count >= MAX_CODE_TOOL_SCOPES_PER_RUN {
        return Err(invalid("code tool scope count exceeds the per-run limit"));
    }
    if state
        .code_tools
        .scopes
        .values()
        .any(|existing| existing.spec.parent_invocation_id == scope.parent_invocation_id)
    {
        return Err(invalid(
            "parent workflow invocation already owns a code tool scope",
        ));
    }
    Ok(())
}

fn validate_call(state: &CoreAgentState, call: &CodeToolCallSpec) -> Result<(), DomainError> {
    validate_id("request_id", &call.origin.request_id)?;
    let scope = state
        .code_tools
        .scopes
        .get(&call.origin.execution_id)
        .ok_or_else(|| invalid("unknown code tool execution scope"))?;
    if scope.closed {
        return Err(invalid("code tool execution scope is closed"));
    }
    validate_live_parent(state, &scope.spec)?;
    if scope.toolset_revision != state.tooling.revision {
        return Err(invalid(
            "code tool scope catalog is stale after a toolset change",
        ));
    }
    if !scope
        .spec
        .bindings
        .get(&call.binding_id)
        .is_some_and(|binding| {
            binding.tool_id == call.tool_id && binding.tool_name == call.tool_name
        })
        || !state
            .tooling
            .tools
            .get(&call.tool_id)
            .is_some_and(|tool| tool.invokes_client_effect())
    {
        return Err(invalid("code tool is outside the execution grant"));
    }
    if scope.calls.len() >= scope.spec.max_calls as usize {
        return Err(invalid("code tool execution exhausted its call budget"));
    }
    if scope
        .calls
        .values()
        .filter(|call| !call.status.is_terminal())
        .count()
        >= scope.spec.max_in_flight as usize
    {
        return Err(invalid("code tool execution exceeded its in-flight limit"));
    }
    if let Some(binding) = state.workflow_tools.binding_for_tool_name(&call.tool_id) {
        let parent = code_tool_parent(state, &scope.spec)?;
        let pending = state
            .code_tools
            .scopes
            .values()
            .filter(|scope| {
                code_tool_parent(state, &scope.spec)
                    .is_ok_and(|owner| owner.run_id == parent.run_id)
            })
            .flat_map(|scope| scope.calls.values())
            .filter(|other| {
                other.spec.tool_id == call.tool_id
                    && matches!(other.status, CodeToolCallStatus::Pending)
            })
            .count();
        if state
            .workflow_tools
            .emission_count(parent.run_id, &binding.definition.tool_id) as usize
            + pending
            >= crate::MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN as usize
        {
            return Err(invalid(
                "workflow tool invoked from code exhausted its emission budget",
            ));
        }
    }
    let call_id = code_tool_call_id(&call.origin);
    if state.runs.active.as_ref().is_some_and(|run| {
        run.tool_batches
            .values()
            .any(|batch| batch.calls.iter().any(|call| call.call.call_id == call_id))
    }) {
        return Err(invalid(
            "code tool call identity collides with a model call",
        ));
    }
    Ok(())
}

fn joins(state: &CoreAgentState, origin: &CodeToolOrigin) -> Result<CoreAgentJoins, DomainError> {
    let scope = state
        .code_tools
        .scopes
        .get(&origin.execution_id)
        .ok_or_else(|| invalid("unknown code tool execution scope"))?;
    let parent = code_tool_parent(state, &scope.spec)?;
    Ok(CoreAgentJoins {
        run_id: Some(parent.run_id),
        turn_id: Some(parent.turn_id),
        tool_batch_id: Some(parent.tool_batch_id),
        tool_call_id: Some(code_tool_call_id(origin)),
        ..Default::default()
    })
}

pub fn open_code_tool_scope_proposals(
    state: &CoreAgentState,
    scope: CodeToolScopeSpec,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    if let Some(existing) = state.code_tools.scopes.get(&scope.execution_id) {
        return if existing.spec == scope {
            Ok(Vec::new())
        } else {
            Err(invalid(
                "code tool execution id reused with a different scope",
            ))
        };
    }
    validate_scope(state, &scope)?;
    Ok(vec![CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEvent::CodeTool(CodeToolEvent::ScopeOpened { scope }),
    )])
}

pub fn admit_code_tool_call_proposals(
    state: &CoreAgentState,
    call: CodeToolCallSpec,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    if let Some(existing) = code_tool_call(state, &call.origin) {
        return if existing.spec == call {
            Ok(Vec::new())
        } else {
            Err(invalid(
                "code tool request id reused with different arguments or binding",
            ))
        };
    }
    validate_call(state, &call)?;
    let promise_id_base = state
        .id_cursors
        .last_promise_id
        .checked_add(1)
        .ok_or_else(|| invalid("promise counter exhausted"))?;
    promise_id_base
        .checked_add(CODE_TOOL_PROMISE_SLOTS - 1)
        .ok_or_else(|| invalid("promise counter exhausted"))?;
    Ok(vec![CoreAgentEventProposal::new(
        joins(state, &call.origin)?,
        CoreAgentEvent::CodeTool(CodeToolEvent::CallAdmitted {
            call,
            promise_id_base,
        }),
    )])
}

pub fn complete_code_tool_call_proposals(
    state: &CoreAgentState,
    origin: CodeToolOrigin,
    result: ToolInvocationResult,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let call =
        code_tool_call(state, &origin).ok_or_else(|| invalid("unknown code tool request"))?;
    validate_result(call, &result)?;
    // A joined preparation outcome records a wait, not its acknowledgement.
    // Redelivery of that same admitted effect is still idempotent, including
    // after the reply has completed the call.
    if !matches!(call.status, CodeToolCallStatus::Pending)
        && result.status == ToolCallStatus::Succeeded
        && result.effects.len() == 1
        && state
            .workflow_tools
            .binding_for_tool_name(&call.spec.tool_id)
            .is_some_and(|binding| {
                matches!(binding.completion, WorkflowToolCompletion::Joined { .. })
            })
        && let Some(invocation) =
            super::workflow_tool::invocation_from_emit_effect(&result.effects[0])?
        && invocation.tool_call_id == call.call_id
        && state
            .workflow_tools
            .emissions
            .get(&invocation.invocation_id)
            .or_else(|| {
                state
                    .workflow_tools
                    .start_requests
                    .get(&invocation.invocation_id)
            })
            == Some(&invocation)
    {
        let deadline =
            super::workflow_tool::completion_deadline_from_emit_effect(&result.effects[0])?;
        if invocation
            .completion_promises
            .as_ref()
            .is_some_and(|promises| {
                promises.values().all(|id| {
                    state
                        .promises
                        .promises
                        .get(id)
                        .is_some_and(|promise| promise.deadline_ms == deadline)
                })
            })
        {
            return Ok(Vec::new());
        }
    }
    if let CodeToolCallStatus::Completed { result: existing } = &call.status {
        return if existing.matches_invocation_result(&result) {
            Ok(Vec::new())
        } else {
            Err(invalid(
                "code tool completion conflicts with the recorded outcome",
            ))
        };
    }
    if !matches!(call.status, CodeToolCallStatus::Pending) {
        return Err(invalid("code tool request is already waiting"));
    }
    validate_result(call, &result)?;
    let scope = &state.code_tools.scopes[&origin.execution_id];
    let parent = code_tool_parent(state, &scope.spec)?;
    crate::core::drive::code_tool_result_proposals(
        state,
        &parent.session_id,
        origin,
        ToolInvocationBatchResult {
            run_id: parent.run_id,
            turn_id: parent.turn_id,
            batch_id: parent.tool_batch_id,
            results: vec![result],
        },
    )
}

fn validate_result(call: &CodeToolCall, result: &ToolInvocationResult) -> Result<(), DomainError> {
    validate_result_fields(call, &result.call_id, result.status)
}

fn validate_result_fields(
    call: &CodeToolCall,
    call_id: &ToolCallId,
    status: ToolCallStatus,
) -> Result<(), DomainError> {
    if call_id != &call.call_id
        || !matches!(
            status,
            ToolCallStatus::Succeeded | ToolCallStatus::Failed | ToolCallStatus::Cancelled
        )
    {
        return Err(invalid(
            "code tool result does not match its admitted call or is not terminal",
        ));
    }
    Ok(())
}

pub fn defer_code_tool_call_proposals(
    state: &CoreAgentState,
    origin: CodeToolOrigin,
    spec: AwaitSpec,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let call =
        code_tool_call(state, &origin).ok_or_else(|| invalid("unknown code tool request"))?;
    if call.spec.tool_id.as_str() != crate::AWAIT_TOOL_ID {
        return Err(invalid(
            "code tool await suspension requires concurrency.await",
        ));
    }
    if spec.promise_ids.is_empty()
        || spec.promise_ids.len() > 32
        || spec.promise_ids.iter().collect::<BTreeSet<_>>().len() != spec.promise_ids.len()
    {
        return Err(invalid(
            "code tool await requires 1..=32 distinct promise ids",
        ));
    }
    let parent = code_tool_parent(state, &state.code_tools.scopes[&origin.execution_id].spec)?;
    crate::core::drive::validate_await_spec_for_active_run(state, parent.run_id, &spec)?;
    let suspension = ToolBatchSuspension::AwaitTool {
        call_id: call.call_id.clone(),
        spec,
    };
    if let CodeToolCallStatus::Waiting {
        suspension: existing,
    } = &call.status
    {
        return if existing == &suspension {
            Ok(Vec::new())
        } else {
            Err(invalid("code tool await changed after admission"))
        };
    }
    if !matches!(call.status, CodeToolCallStatus::Pending) {
        return Err(invalid("code tool request already completed"));
    }
    Ok(vec![CoreAgentEventProposal::new(
        joins(state, &origin)?,
        CoreAgentEvent::CodeTool(CodeToolEvent::CallDeferred { origin, suspension }),
    )])
}

pub fn code_tool_wake(
    state: &CoreAgentState,
    origin: &CodeToolOrigin,
    now_ms: u64,
) -> Option<WakeReason> {
    let scope = state.code_tools.scopes.get(&origin.execution_id)?;
    let call = scope.calls.get(&origin.request_id)?;
    let CodeToolCallStatus::Waiting { suspension } = &call.status else {
        return None;
    };
    if scope.cancel_requested || validate_live_parent(state, &scope.spec).is_err() {
        return Some(WakeReason::Cancelled);
    }
    let spec = suspension.spec();
    if spec
        .deadline_at_ms
        .is_some_and(|deadline| deadline <= now_ms)
    {
        return Some(WakeReason::Timeout);
    }
    let terminal = spec
        .promise_ids
        .iter()
        .filter(|id| {
            state
                .promises
                .promises
                .get(*id)
                .is_some_and(|promise| promise.status.is_terminal())
        })
        .count();
    match spec.mode {
        AwaitMode::All if terminal == spec.promise_ids.len() => Some(WakeReason::Terminal),
        AwaitMode::Any if terminal > 0 => Some(WakeReason::Terminal),
        _ => None,
    }
}

/// Project one joined reply without disturbing the outer model batch.
pub fn code_tool_joined_result(
    state: &CoreAgentState,
    origin: &CodeToolOrigin,
    cancel_pending: bool,
) -> Result<ToolInvocationResult, DomainError> {
    let call = code_tool_call(state, origin).ok_or_else(|| invalid("unknown code tool request"))?;
    let CodeToolCallStatus::Waiting {
        suspension: ToolBatchSuspension::JoinedWorkflowCalls { calls, .. },
    } = &call.status
    else {
        return Err(invalid(
            "code tool request is not waiting for a joined reply",
        ));
    };
    let promise = state
        .promises
        .promises
        .get(&calls[0].promise_id)
        .ok_or_else(|| invalid("code tool joined promise is missing"))?;
    let (status, output_ref, error_ref) = match promise.status {
        PromiseStatus::Resolved => (ToolCallStatus::Succeeded, promise.payload_ref.clone(), None),
        PromiseStatus::Failed => (
            ToolCallStatus::Failed,
            None,
            Some(
                promise
                    .error_ref
                    .clone()
                    .unwrap_or_else(crate::unavailable_tool_result_ref),
            ),
        ),
        PromiseStatus::Cancelled => (
            ToolCallStatus::Cancelled,
            None,
            Some(crate::cancelled_tool_result_ref()),
        ),
        PromiseStatus::Pending if cancel_pending => (
            ToolCallStatus::Cancelled,
            None,
            Some(crate::cancelled_tool_result_ref()),
        ),
        PromiseStatus::Pending => return Err(invalid("code tool joined promise is still pending")),
    };
    Ok(ToolInvocationResult {
        call_id: call.call_id.clone(),
        status,
        output_ref,
        error_ref,
        model_visible_context_entries: Vec::new(),
        effects: Vec::new(),
        attachments: Vec::new(),
        duration_ms: None,
        output_bytes: None,
        truncated: false,
    })
}

pub fn resume_code_tool_call_proposals(
    state: &CoreAgentState,
    origin: CodeToolOrigin,
    result: ToolInvocationResult,
    claim_observed_at_ms: u64,
    observed_at_ms: u64,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let call =
        code_tool_call(state, &origin).ok_or_else(|| invalid("unknown code tool request"))?;
    if let CodeToolCallStatus::Completed { result: existing } = &call.status {
        return if existing.matches_invocation_result(&result) {
            Ok(Vec::new())
        } else {
            Err(invalid("code tool resume conflicts with recorded result"))
        };
    }
    validate_result(call, &result)?;
    let wake = code_tool_wake(state, &origin, claim_observed_at_ms);
    if claim_observed_at_ms > observed_at_ms || wake.is_none() {
        return Err(invalid("code tool resume has no satisfied wake"));
    }
    let mut proposals = Vec::new();
    if !result.effects.is_empty() {
        return Err(invalid("code tool resume cannot introduce tool effects"));
    }
    if let CodeToolCallStatus::Waiting {
        suspension: ToolBatchSuspension::JoinedWorkflowCalls { calls, .. },
    } = &call.status
    {
        let promise = &state.promises.promises[&calls[0].promise_id];
        let expected_status = match promise.status {
            PromiseStatus::Resolved => ToolCallStatus::Succeeded,
            PromiseStatus::Failed => ToolCallStatus::Failed,
            PromiseStatus::Pending | PromiseStatus::Cancelled => ToolCallStatus::Cancelled,
        };
        if result.status != expected_status
            || (promise.status == PromiseStatus::Resolved
                && result.output_ref != promise.payload_ref)
        {
            return Err(invalid(
                "code tool joined result does not match its completion promise",
            ));
        }
        // Scope closure can precede a still-running workflow preparation.
        // Its late effect creates the reply promise after close had nothing
        // to cancel. Settling that wait must emit the usual cancellation fact
        // so the receiver/owned workflow is cancelled as well as the waiter.
        // Already terminal replies remain authoritative; model-owned submitted
        // promises belong to separate acknowledged calls and are untouched.
        if matches!(wake, Some(WakeReason::Cancelled | WakeReason::Timeout)) {
            for joined in calls {
                if state
                    .promises
                    .promises
                    .get(&joined.promise_id)
                    .is_some_and(|promise| {
                        promise.status == PromiseStatus::Pending
                            && promise.ownership == PromiseOwnership::Runtime
                    })
                {
                    proposals.push(CoreAgentEventProposal::new(
                        joins(state, &origin)?,
                        CoreAgentEvent::Promise(PromiseEvent::Cancelled {
                            promise_id: joined.promise_id.clone(),
                        }),
                    ));
                }
            }
        }
    }
    proposals.push(CoreAgentEventProposal::new(
        joins(state, &origin)?,
        CoreAgentEvent::CodeTool(CodeToolEvent::CallCompleted {
            origin,
            result: result.into(),
        }),
    ));
    Ok(proposals)
}

pub fn close_code_tool_scope_proposals(
    state: &CoreAgentState,
    execution_id: String,
    cancel: bool,
) -> Result<Vec<CoreAgentEventProposal>, DomainError> {
    let scope = state
        .code_tools
        .scopes
        .get(&execution_id)
        .ok_or_else(|| invalid("unknown code tool execution scope"))?;
    if scope.closed && (!cancel || scope.cancel_requested) {
        return Ok(Vec::new());
    }
    let mut proposals = vec![CoreAgentEventProposal::new(
        CoreAgentJoins::default(),
        CoreAgentEvent::CodeTool(CodeToolEvent::ScopeClosed {
            execution_id,
            cancel,
        }),
    )];
    if cancel {
        for call in scope.calls.values() {
            if let CodeToolCallStatus::Waiting {
                suspension: ToolBatchSuspension::JoinedWorkflowCalls { calls, .. },
            } = &call.status
            {
                for joined in calls {
                    if state
                        .promises
                        .promises
                        .get(&joined.promise_id)
                        .is_some_and(|promise| {
                            promise.status == PromiseStatus::Pending
                                && promise.ownership == PromiseOwnership::Runtime
                        })
                    {
                        proposals.push(CoreAgentEventProposal::new(
                            joins(state, &call.spec.origin)?,
                            CoreAgentEvent::Promise(PromiseEvent::Cancelled {
                                promise_id: joined.promise_id.clone(),
                            }),
                        ));
                    }
                }
            }
        }
    }
    Ok(proposals)
}

/// Reuse the ordinary request projection without opening another model batch.
pub fn code_tool_request(
    session_id: &SessionId,
    state: &CoreAgentState,
    execution_id: &str,
    request_id: &str,
) -> Result<ToolInvocationBatchRequest, DomainError> {
    let origin = CodeToolOrigin {
        execution_id: execution_id.to_owned(),
        request_id: request_id.to_owned(),
    };
    let call =
        code_tool_call(state, &origin).ok_or_else(|| invalid("unknown code tool request"))?;
    if !matches!(call.status, CodeToolCallStatus::Pending) {
        return Err(invalid("code tool request is not pending"));
    }
    let scope = &state.code_tools.scopes[execution_id];
    if scope.cancel_requested {
        return Err(invalid("code tool scope is cancelling"));
    }
    validate_live_parent(state, &scope.spec)?;
    if scope.toolset_revision != state.tooling.revision {
        return Err(invalid(
            "code tool scope catalog is stale after a toolset change",
        ));
    }
    let parent = code_tool_parent(state, &scope.spec)?;
    if &parent.session_id != session_id {
        return Err(invalid(
            "code tool request session does not match its parent",
        ));
    }
    let mut request = crate::core::drive::tool_invocation_request(
        state,
        session_id,
        parent.run_id,
        parent.turn_id,
        parent.tool_batch_id,
        call.promise_id_base,
        std::slice::from_ref(&crate::ObservedToolCall {
            call_id: call.call_id.clone(),
            tool_id: Some(call.spec.tool_id.clone()),
            tool_name: call.spec.tool_name.clone(),
            provider_kind: None,
            arguments_ref: call.spec.arguments_ref.clone(),
            native_call_ref: None,
        }),
    )?;
    if let Some(remote) = &mut request.calls[0].remote_mcp {
        match remote {
            crate::RemoteMcpCallRuntime::Injected {
                approval_decision, ..
            }
            | crate::RemoteMcpCallRuntime::Search {
                approval_decision, ..
            } => *approval_decision = None,
        }
    }
    Ok(request)
}

pub(crate) fn apply_code_tool_event(
    state: &mut CoreAgentState,
    event: &CodeToolEvent,
) -> Result<(), DomainError> {
    match event {
        CodeToolEvent::ScopeOpened { scope } => {
            if state.code_tools.scopes.contains_key(&scope.execution_id) {
                return Err(invalid("duplicate code tool scope event"));
            }
            validate_scope(state, scope)?;
            // Retain closed outcomes for the current run, and drop prior-run
            // scope indexes. The immutable log remains the outcome archive.
            let run_id = code_tool_parent(state, scope)?.run_id;
            let retain: BTreeSet<_> = state
                .code_tools
                .scopes
                .iter()
                .filter(|(_, existing)| {
                    code_tool_parent(state, &existing.spec)
                        .is_ok_and(|parent| parent.run_id == run_id)
                })
                .map(|(id, _)| id.clone())
                .collect();
            state.code_tools.scopes.retain(|id, _| retain.contains(id));
            state.code_tools.scopes.insert(
                scope.execution_id.clone(),
                CodeToolScope {
                    toolset_revision: state.tooling.revision,
                    spec: scope.clone(),
                    closed: false,
                    cancel_requested: false,
                    calls: BTreeMap::new(),
                },
            );
        }
        CodeToolEvent::CallAdmitted {
            call,
            promise_id_base,
        } => {
            validate_call(state, call)?;
            if code_tool_call(state, &call.origin).is_some() {
                return Err(invalid("duplicate code tool admission event"));
            }
            if Some(*promise_id_base) != state.id_cursors.last_promise_id.checked_add(1) {
                return Err(invalid("code tool promise reservation is not contiguous"));
            }
            let last = promise_id_base
                .checked_add(CODE_TOOL_PROMISE_SLOTS - 1)
                .ok_or_else(|| invalid("promise counter exhausted"))?;
            state
                .code_tools
                .scopes
                .get_mut(&call.origin.execution_id)
                .expect("validated scope")
                .calls
                .insert(
                    call.origin.request_id.clone(),
                    CodeToolCall {
                        spec: call.clone(),
                        call_id: code_tool_call_id(&call.origin),
                        promise_id_base: *promise_id_base,
                        status: CodeToolCallStatus::Pending,
                    },
                );
            state.id_cursors.last_promise_id = last;
        }
        CodeToolEvent::CallCompleted { origin, result } => {
            let call = code_tool_call(state, origin)
                .ok_or_else(|| invalid("unknown code tool request"))?;
            validate_result_fields(call, &result.call_id, result.status)?;
            if call.status.is_terminal() {
                return Err(invalid("duplicate code tool completion event"));
            }
            state
                .code_tools
                .scopes
                .get_mut(&origin.execution_id)
                .expect("validated scope")
                .calls
                .get_mut(&origin.request_id)
                .expect("validated call")
                .status = CodeToolCallStatus::Completed {
                result: result.clone(),
            };
        }
        CodeToolEvent::CallDeferred { origin, suspension } => {
            let call = code_tool_call(state, origin)
                .ok_or_else(|| invalid("unknown code tool request"))?;
            if !matches!(call.status, CodeToolCallStatus::Pending) {
                return Err(invalid(
                    "code tool request was already deferred or completed",
                ));
            }
            match suspension {
                ToolBatchSuspension::AwaitTool { call_id, spec } => {
                    if call_id != &call.call_id {
                        return Err(invalid("code tool await call id mismatch"));
                    }
                    defer_code_tool_call_proposals(state, origin.clone(), spec.clone())?;
                }
                ToolBatchSuspension::JoinedWorkflowCalls { calls, spec } => {
                    if calls.len() != 1
                        || calls[0].call_id != call.call_id
                        || spec.promise_ids != vec![calls[0].promise_id.clone()]
                        || spec.mode != AwaitMode::All
                    {
                        return Err(invalid(
                            "code tool joined suspension does not match its call",
                        ));
                    }
                    let binding = state
                        .workflow_tools
                        .binding_for_tool_name(&call.spec.tool_id)
                        .ok_or_else(|| invalid("code tool joined binding is missing"))?;
                    if !matches!(binding.completion, WorkflowToolCompletion::Joined { .. }) {
                        return Err(invalid("code tool suspension requires joined completion"));
                    }
                    let promise = state
                        .promises
                        .promises
                        .get(&calls[0].promise_id)
                        .ok_or_else(|| invalid("code tool completion promise is missing"))?;
                    let parent = code_tool_parent(
                        state,
                        &state.code_tools.scopes[&origin.execution_id].spec,
                    )?;
                    let expected_invocation_id = WorkflowToolInvocationId::for_call(
                        binding.session_universe_id,
                        &parent.session_id,
                        parent.run_id,
                        parent.turn_id,
                        parent.tool_batch_id,
                        &call.call_id,
                        &binding.binding_fingerprint,
                    );
                    if calls[0].invocation_id != expected_invocation_id
                        || promise.ownership != PromiseOwnership::Runtime
                        || promise.scope
                            != (crate::PromiseScope::Run {
                                run_id: parent.run_id,
                            })
                        || promise.status != PromiseStatus::Pending
                        || promise.deadline_ms.is_none_or(|deadline| deadline == 0)
                        || !matches!(&promise.source, crate::PromiseSource::Workflow { invocation_id, completion_key, .. }
                            if invocation_id == expected_invocation_id.as_str() && completion_key == crate::REPLY_COMPLETION_KEY)
                        || calls[0]
                            .promise_id
                            .number()
                            .checked_sub(call.promise_id_base)
                            .is_none_or(|offset| offset >= CODE_TOOL_PROMISE_SLOTS)
                    {
                        return Err(invalid(
                            "code tool joined promise does not match its admitted invocation",
                        ));
                    }
                }
            }
            state
                .code_tools
                .scopes
                .get_mut(&origin.execution_id)
                .expect("validated scope")
                .calls
                .get_mut(&origin.request_id)
                .expect("validated call")
                .status = CodeToolCallStatus::Waiting {
                suspension: suspension.clone(),
            };
        }
        CodeToolEvent::ScopeClosed {
            execution_id,
            cancel,
        } => {
            let scope = state
                .code_tools
                .scopes
                .get_mut(execution_id)
                .ok_or_else(|| invalid("unknown code tool execution scope"))?;
            scope.closed = true;
            scope.cancel_requested |= cancel;
        }
    }
    Ok(())
}
