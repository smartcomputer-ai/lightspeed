//! Session-owned admission and independent dispatch of code tool effects.
//! Update handlers and the dispatcher only queue work. This module's main-loop
//! drain is the sole writer of code tool facts, through the ordinary event append.

use super::*;
use crate::workflows::WorkflowContextExt as _;
use crate::{CodeToolRejection, CodeToolRejectionKind as Rejection};
use harness::{CodeToolCallStatus, CodeToolOrigin, ToolCallStatus, ToolParallelism};
use std::{pin::Pin, task::Poll};
use temporalio_sdk::{ActivityExecutionError, CancellableFuture};

type Key = (String, String);
type AdmissionResult = Result<(), CodeToolRejection>;
const MAX_UPDATE_WAITERS: usize = 256;

#[derive(Default)]
pub(super) struct CodeToolExecutionState {
    next_ticket: u64,
    waiters: usize,
    pending: Vec<(u64, Request)>,
    receipts: BTreeMap<u64, AdmissionResult>,
    completions: Vec<(CodeToolOrigin, crate::CodeToolInvokeActivityResult)>,
    // Kept until the result is appended, so a ready future cannot redispatch.
    inflight: BTreeMap<Key, bool>,
}

enum Request {
    Open(crate::OpenCodeToolScopeRequest),
    Invoke(crate::InvokeCodeToolRequest),
    Close(crate::CloseCodeToolScopeRequest),
}

fn rejected(kind: Rejection, message: impl Into<String>) -> CodeToolRejection {
    CodeToolRejection::new(kind, message)
}

fn key(origin: &CodeToolOrigin) -> Key {
    (origin.execution_id.clone(), origin.request_id.clone())
}

fn enqueue(state: &mut AgentSessionWorkflow, request: Request) -> Result<u64, CodeToolRejection> {
    if !state.ready || state.core_state.lifecycle.status != CoreAgentStatus::Open {
        return Err(rejected(
            Rejection::SessionNotReady,
            "session is not accepting code tool requests",
        ));
    }
    if state.code_tools.waiters >= MAX_UPDATE_WAITERS {
        return Err(rejected(
            Rejection::LimitExceeded,
            "too many code tool Update waiters",
        ));
    }
    state.code_tools.next_ticket += 1;
    let ticket = state.code_tools.next_ticket;
    state.code_tools.waiters += 1;
    state.code_tools.pending.push((ticket, request));
    Ok(ticket)
}

async fn await_admission(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    ticket: u64,
) -> AdmissionResult {
    ctx.wait_for_state(move |state| state.code_tools.receipts.contains_key(&ticket))
        .await;
    ctx.state_mut(|state| {
        state
            .code_tools
            .receipts
            .remove(&ticket)
            .expect("ready receipt")
    })
}

pub(super) async fn open(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: crate::OpenCodeToolScopeRequest,
) -> crate::CodeToolScopeResult {
    let execution_id = request.execution_id.clone();
    let ticket = ctx.state_mut(|state| enqueue(state, Request::Open(request)))?;
    let admitted = await_admission(ctx, ticket).await;
    ctx.state_mut(|state| state.code_tools.waiters -= 1);
    admitted?;
    ctx.state(|state| report(state, &execution_id))
}

pub(super) async fn invoke(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: crate::InvokeCodeToolRequest,
) -> crate::CodeToolInvocationResult {
    let origin = CodeToolOrigin {
        execution_id: request.execution_id.clone(),
        request_id: request.request_id.clone(),
    };
    let ticket = ctx.state_mut(|state| enqueue(state, Request::Invoke(request)))?;
    let admitted = await_admission(ctx, ticket).await;
    let result = if let Err(error) = admitted {
        Err(error)
    } else {
        let wanted = origin.clone();
        ctx.wait_for_state(move |state| {
            harness::code_tool_call(&state.core_state, &wanted)
                .is_some_and(|call| call.status.is_terminal())
        })
        .await;
        ctx.state(|state| {
            harness::code_tool_call(&state.core_state, &origin)
                .map(crate::CodeToolCallOutcome::from)
                .ok_or_else(|| rejected(Rejection::UnknownScope, "code tool result unavailable"))
        })
    };
    ctx.state_mut(|state| state.code_tools.waiters -= 1);
    result
}

pub(super) async fn close(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: crate::CloseCodeToolScopeRequest,
) -> crate::CodeToolScopeResult {
    let execution_id = request.execution_id.clone();
    let ticket = ctx.state_mut(|state| enqueue(state, Request::Close(request)))?;
    let admitted = await_admission(ctx, ticket).await;
    ctx.state_mut(|state| state.code_tools.waiters -= 1);
    admitted?;
    ctx.state(|state| report(state, &execution_id))
}

pub(super) fn report(
    state: &AgentSessionWorkflow,
    execution_id: &str,
) -> crate::CodeToolScopeResult {
    state
        .core_state
        .code_tools
        .scopes
        .get(execution_id)
        .map(crate::CodeToolScopeReport::from)
        .ok_or_else(|| rejected(Rejection::UnknownScope, "unknown code tool execution scope"))
}

/// All durable mutations run on the session loop, including completions that
/// raced a scope close. Infrastructure append failures still fail the workflow.
pub(super) async fn process_pending(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let completions = ctx.state_mut(|state| std::mem::take(&mut state.code_tools.completions));
    for (origin, outcome) in completions {
        if ctx.state(|state| {
            harness::code_tool_call(&state.core_state, &origin)
                .is_some_and(|call| call.status.is_terminal())
        }) {
            // Forced run termination records an unavailable outcome. A late
            // activity reply cannot resurrect the run or reapply its effects.
            ctx.state_mut(|state| {
                state.code_tools.inflight.remove(&key(&origin));
            });
            continue;
        }
        let command = match outcome {
            crate::CodeToolInvokeActivityResult::Completed { result } => {
                CoreAgentCommand::CompleteCodeToolCall {
                    origin: origin.clone(),
                    result,
                }
            }
            crate::CodeToolInvokeActivityResult::Deferred { spec } => {
                CoreAgentCommand::DeferCodeToolCall {
                    origin: origin.clone(),
                    spec,
                }
            }
            crate::CodeToolInvokeActivityResult::EnvironmentNotReady { .. } => {
                unreachable!("dispatcher handles readiness")
            }
        };
        if let Err(error) = apply(ctx, command).await? {
            // Invalid wait targets or runtime result facts fail this call, not
            // the parent workflow. Admission did not append partial effects.
            let error_ref = tool_batches::put_boundary_error_blob(ctx, &error.message).await;
            apply_internal(
                ctx,
                CoreAgentCommand::CompleteCodeToolCall {
                    origin: origin.clone(),
                    result: tool_batches::boundary_call_result(
                        harness::code_tool_call_id(&origin),
                        ToolCallStatus::Failed,
                        error_ref,
                    ),
                },
            )
            .await?;
        }
        ctx.state_mut(|state| {
            state.code_tools.inflight.remove(&key(&origin));
        });
    }
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.code_tools.pending));
    for (ticket, request) in pending {
        let result = match request {
            Request::Open(request) => admit_open(ctx, request).await?,
            Request::Invoke(request) => {
                let command = ctx.state(|state| invocation_command(state, request));
                match command {
                    Ok(command) => apply(ctx, command).await?,
                    Err(error) => Err(error),
                }
            }
            Request::Close(request) => {
                apply(
                    ctx,
                    CoreAgentCommand::CloseCodeToolScope {
                        execution_id: request.execution_id,
                        cancel: request.cancel_pending,
                    },
                )
                .await?
            }
        };
        ctx.state_mut(|state| {
            state.code_tools.receipts.insert(ticket, result);
        });
    }
    let expired = ctx.state(|state| {
        state
            .core_state
            .code_tools
            .scopes
            .values()
            .filter(|scope| {
                !scope.cancel_requested
                    && !harness::code_tool_scope_is_live(&state.core_state, &scope.spec)
            })
            .map(|scope| scope.spec.execution_id.clone())
            .collect::<Vec<_>>()
    });
    for execution_id in expired {
        apply_internal(
            ctx,
            CoreAgentCommand::CloseCodeToolScope {
                execution_id,
                cancel: true,
            },
        )
        .await?;
    }
    reconcile_waits(ctx).await
}

async fn apply(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    command: CoreAgentCommand,
) -> anyhow::Result<AdmissionResult> {
    let mut drive = drive_from_state(ctx)?;
    match admit_and_append_command(ctx, &mut drive, command, None).await? {
        CommandAdmissionResult::Accepted => Ok(Ok(())),
        CommandAdmissionResult::Rejected(failure) => {
            Ok(Err(rejected(Rejection::InvalidRequest, failure.message)))
        }
    }
}

async fn apply_internal(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    command: CoreAgentCommand,
) -> anyhow::Result<()> {
    apply(ctx, command)
        .await?
        .map_err(|error| anyhow::anyhow!("code tool completion rejected: {}", error.message))
}

fn invocation_command(
    state: &AgentSessionWorkflow,
    request: crate::InvokeCodeToolRequest,
) -> Result<CoreAgentCommand, CodeToolRejection> {
    let scope = state
        .core_state
        .code_tools
        .scopes
        .get(&request.execution_id)
        .ok_or_else(|| rejected(Rejection::UnknownScope, "unknown code tool scope"))?;
    if let Some(existing) = scope.calls.get(&request.request_id) {
        if existing.spec.binding_id != request.binding_id
            || existing.spec.arguments_ref != request.arguments_ref
        {
            return Err(rejected(
                Rejection::Conflict,
                "request identity already names different arguments or binding",
            ));
        }
    } else {
        if scope.closed {
            return Err(rejected(
                Rejection::ScopeClosed,
                "code tool execution scope is closed",
            ));
        }
        if scope.calls.len() >= scope.spec.max_calls as usize
            || scope
                .calls
                .values()
                .filter(|call| !call.status.is_terminal())
                .count()
                >= scope.spec.max_in_flight as usize
        {
            return Err(rejected(
                Rejection::LimitExceeded,
                "code tool execution call budget exhausted",
            ));
        }
    }
    let binding = scope
        .spec
        .bindings
        .get(&request.binding_id)
        .ok_or_else(|| {
            rejected(
                Rejection::PermissionDenied,
                "binding is not granted to this scope",
            )
        })?;
    Ok(CoreAgentCommand::AdmitCodeToolCall {
        call: harness::CodeToolCallSpec {
            origin: CodeToolOrigin {
                execution_id: request.execution_id,
                request_id: request.request_id,
            },
            binding_id: request.binding_id,
            tool_id: binding.tool_id.clone(),
            tool_name: binding.tool_name.clone(),
            arguments_ref: request.arguments_ref,
        },
    })
}

async fn admit_open(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: crate::OpenCodeToolScopeRequest,
) -> anyhow::Result<AdmissionResult> {
    if let Some(existing) = ctx.state(|state| {
        state
            .core_state
            .code_tools
            .scopes
            .get(&request.execution_id)
            .cloned()
    }) {
        let ids = existing
            .spec
            .bindings
            .values()
            .map(|binding| binding.tool_id.clone())
            .collect::<BTreeSet<_>>();
        return Ok(
            if existing.spec.parent_invocation_id == request.parent_invocation_id
                && existing.spec.max_calls == request.max_calls
                && existing.spec.max_in_flight == request.max_in_flight
                && request
                    .allowed_tools
                    .as_ref()
                    .is_none_or(|allowed| &ids == allowed)
            {
                Ok(())
            } else {
                Err(rejected(
                    Rejection::Conflict,
                    "execution identity already has a different scope",
                ))
            },
        );
    }
    if request
        .allowed_tools
        .as_ref()
        .is_some_and(|allowed| allowed.len() > 4096)
        || request.max_calls == 0
        || request.max_calls > harness::MAX_CODE_TOOL_CALLS_PER_SCOPE
        || request.max_in_flight == 0
        || request.max_in_flight > harness::MAX_CODE_TOOL_IN_FLIGHT
        || request.max_in_flight > request.max_calls
    {
        return Ok(Err(rejected(
            Rejection::LimitExceeded,
            "code tool scope exceeds limits",
        )));
    }
    let prepared = ctx.state(|state| {
        let run = state.core_state.runs.active.as_ref().ok_or_else(|| {
            rejected(
                Rejection::Unavailable,
                "code tool scope requires active parent",
            )
        })?;
        let model = run
            .run_config
            .model_override
            .clone()
            .or_else(|| {
                state
                    .core_state
                    .lifecycle
                    .config
                    .as_ref()
                    .map(|config| config.model.clone())
            })
            .ok_or_else(|| rejected(Rejection::SessionNotReady, "session has no model"))?;
        let parent = state
            .core_state
            .workflow_tools
            .start_requests
            .get(&request.parent_invocation_id)
            .or_else(|| {
                state
                    .core_state
                    .workflow_tools
                    .emissions
                    .get(&request.parent_invocation_id)
            })
            .and_then(|parent| {
                state
                    .core_state
                    .workflow_tools
                    .bindings
                    .get(&parent.tool_id)
            })
            .ok_or_else(|| {
                rejected(
                    Rejection::PermissionDenied,
                    "parent workflow tool is not admitted",
                )
            })?;
        let tools = select_code_tools(
            &state.core_state.tooling.tools,
            &parent.definition.tool.name,
            request.allowed_tools.as_ref(),
        )?;
        Ok::<_, CodeToolRejection>((
            crate::CodeToolPrepareScopeActivityRequest { tools, model },
            state.core_state.tooling.revision,
        ))
    });
    let (prepared, revision) = match prepared {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    let bindings = match ctx
        .execute_activity(
            WorkflowActivities::code_tool_prepare_scope,
            prepared,
            crate::activity_options(),
        )
        .await
    {
        Ok(result) => result.bindings,
        Err(error) => {
            return Ok(Err(rejected(
                Rejection::Unavailable,
                format!("prepare code tool bindings: {error}"),
            )));
        }
    };
    if ctx.state(|state| state.core_state.tooling.revision) != revision {
        return Ok(Err(rejected(
            Rejection::Conflict,
            "tool registry changed while preparing scope",
        )));
    }
    if request.allowed_tools.as_ref().is_some_and(|allowed| {
        bindings
            .values()
            .map(|binding| binding.tool_id.clone())
            .collect::<BTreeSet<_>>()
            != *allowed
    }) {
        return Ok(Err(rejected(
            Rejection::PermissionDenied,
            "requested tools lack callable bindings",
        )));
    }
    apply(
        ctx,
        CoreAgentCommand::OpenCodeToolScope {
            scope: harness::CodeToolScopeSpec {
                execution_id: request.execution_id,
                parent_invocation_id: request.parent_invocation_id,
                bindings,
                max_calls: request.max_calls,
                max_in_flight: request.max_in_flight,
            },
        },
    )
    .await
}

fn select_code_tools(
    tools: &BTreeMap<harness::ToolName, harness::ToolSpec>,
    parent: &harness::ToolName,
    allowed: Option<&BTreeSet<harness::ToolName>>,
) -> Result<Vec<harness::ToolSpec>, CodeToolRejection> {
    match allowed {
        Some(allowed) => allowed
            .iter()
            .map(|id| {
                tools
                    .get(id)
                    .filter(|tool| &tool.name != parent)
                    .cloned()
                    .ok_or_else(|| {
                        rejected(
                            Rejection::PermissionDenied,
                            "tool is not granted for code tool execution",
                        )
                    })
            })
            .collect(),
        None => Ok(tools
            .values()
            .filter(|tool| &tool.name != parent && tool.invokes_client_effect())
            .cloned()
            .collect()),
    }
}

async fn reconcile_waits(ctx: &mut WorkflowContext<AgentSessionWorkflow>) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let ready = ctx.state(|state| {
        state
            .core_state
            .code_tools
            .scopes
            .values()
            .flat_map(|scope| {
                scope.calls.values().filter_map(|call| {
                    if let CodeToolCallStatus::Waiting { suspension } = &call.status {
                        harness::code_tool_wake(&state.core_state, &call.spec.origin, now)
                            .map(|wake| (call.spec.origin.clone(), suspension.clone(), wake))
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>()
    });
    for (origin, suspension, wake) in ready {
        let results =
            ctx.state(|state| awaits::promise_snapshot(suspension.spec(), &state.core_state));
        let result = match &suspension {
            harness::ToolBatchSuspension::AwaitTool { call_id, .. } => {
                let materialized = ctx
                    .execute_activity(
                        WorkflowActivities::materialize_await_result,
                        crate::AwaitMaterializationRequest {
                            outcome: match wake {
                                harness::WakeReason::Cancelled => crate::AwaitOutcome::Cancelled,
                                harness::WakeReason::Timeout => crate::AwaitOutcome::Timeout,
                                harness::WakeReason::Terminal => crate::AwaitOutcome::Terminal,
                            },
                            results,
                        },
                        crate::activity_options(),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                harness::ToolInvocationResult {
                    call_id: call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(materialized.result_ref),
                    error_ref: None,
                    effects: vec![],
                    model_visible_context_entries: vec![],
                    attachments: materialized.attachments,
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                }
            }
            harness::ToolBatchSuspension::JoinedWorkflowCalls { .. } => {
                let supplements = ctx
                    .execute_activity(
                        WorkflowActivities::prepare_joined_context,
                        crate::JoinedContextPreparationRequest { results },
                        crate::activity_options(),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                let mut result = ctx.state(|state| {
                    harness::code_tool_joined_result(
                        &state.core_state,
                        &origin,
                        wake != harness::WakeReason::Terminal,
                    )
                })?;
                result.attachments = supplements
                    .into_iter()
                    .flat_map(|item| item.attachments)
                    .collect();
                result
            }
        };
        apply_internal(
            ctx,
            CoreAgentCommand::ResumeCodeToolCall {
                origin,
                result,
                claim_observed_at_ms: now,
            },
        )
        .await?;
    }
    Ok(())
}

pub(super) fn has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    !state.code_tools.pending.is_empty()
        || !state.code_tools.completions.is_empty()
        || state.core_state.code_tools.scopes.values().any(|scope| {
            (!scope.cancel_requested
                && !harness::code_tool_scope_is_live(&state.core_state, &scope.spec))
                || scope.calls.values().any(|call| {
                    matches!(call.status, CodeToolCallStatus::Waiting { .. })
                        && harness::code_tool_wake(&state.core_state, &call.spec.origin, 0)
                            .is_some()
                })
        })
}

pub(super) fn nearest_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .core_state
        .code_tools
        .scopes
        .values()
        .flat_map(|scope| scope.calls.values())
        .filter_map(|call| match &call.status {
            CodeToolCallStatus::Waiting { suspension } => suspension.spec().deadline_at_ms,
            _ => None,
        })
        .min()
}

pub(super) fn is_quiescent(state: &AgentSessionWorkflow) -> bool {
    state.code_tools.waiters == 0
        && state.code_tools.pending.is_empty()
        && state.code_tools.completions.is_empty()
        && state.code_tools.inflight.is_empty()
}

pub(super) fn blocks_parent_resume(state: &AgentSessionWorkflow) -> bool {
    state
        .core_state
        .code_tools
        .scopes
        .values()
        .any(|scope| scope.calls.values().any(|call| !call.status.is_terminal()))
}

fn next_dispatch(state: &AgentSessionWorkflow) -> Option<(CodeToolOrigin, bool)> {
    if state.code_tools.inflight.len() >= crate::MAX_CONCURRENT_TOOL_CALLS_PER_BATCH
        || state
            .code_tools
            .inflight
            .values()
            .any(|exclusive| *exclusive)
    {
        return None;
    }
    let call = state
        .core_state
        .code_tools
        .scopes
        .values()
        .flat_map(|scope| scope.calls.values())
        .filter(|call| {
            matches!(call.status, CodeToolCallStatus::Pending)
                && !state
                    .code_tools
                    .inflight
                    .contains_key(&key(&call.spec.origin))
        })
        .min_by_key(|call| call.promise_id_base)?;
    let exclusive = state
        .core_state
        .tooling
        .tools
        .get(&call.spec.tool_id)
        .is_none_or(|tool| tool.parallelism != ToolParallelism::ParallelSafe);
    (!exclusive || state.code_tools.inflight.is_empty())
        .then(|| (call.spec.origin.clone(), exclusive))
}

type ActivityFuture<'a, T> =
    Pin<Box<dyn CancellableFuture<Output = Result<T, ActivityExecutionError>> + 'a>>;
enum TaskFuture<'a> {
    Invoke(ActivityFuture<'a, crate::CodeToolInvokeActivityResult>),
    Readiness(ActivityFuture<'a, crate::AwaitEnvironmentReadyActivityResult>),
    PrepareControls(ActivityFuture<'a, harness::PromiseControlArgumentFacts>),
}
enum TaskOutcome {
    Invoke(Result<crate::CodeToolInvokeActivityResult, ActivityExecutionError>),
    Readiness(Result<crate::AwaitEnvironmentReadyActivityResult, ActivityExecutionError>),
    PrepareControls(Result<harness::PromiseControlArgumentFacts, ActivityExecutionError>),
}
impl TaskFuture<'_> {
    fn cancel(&self) {
        match self {
            Self::Invoke(future) => future.as_ref().get_ref().cancel(),
            Self::Readiness(future) => future.as_ref().get_ref().cancel(),
            Self::PrepareControls(future) => future.as_ref().get_ref().cancel(),
        }
    }
    fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<TaskOutcome> {
        match self {
            Self::Invoke(future) => future.as_mut().poll(cx).map(TaskOutcome::Invoke),
            Self::Readiness(future) => future.as_mut().poll(cx).map(TaskOutcome::Readiness),
            Self::PrepareControls(future) => {
                future.as_mut().poll(cx).map(TaskOutcome::PrepareControls)
            }
        }
    }
}
struct Inflight<'a> {
    origin: CodeToolOrigin,
    request: harness::ToolInvocationCallRequest,
    future: TaskFuture<'a>,
    cancel_sent: bool,
    readiness_attempted: bool,
}
fn start_invoke<'a>(
    ctx: &'a WorkflowContext<AgentSessionWorkflow>,
    request: &harness::ToolInvocationCallRequest,
) -> TaskFuture<'a> {
    TaskFuture::Invoke(Box::pin(ctx.execute_activity(
        WorkflowActivities::code_tool_invoke,
        crate::CodeToolInvokeActivityRequest {
            request: request.clone(),
        },
        crate::tool_call_activity_options(request.execution),
    )))
}
fn scope_cancelled(state: &AgentSessionWorkflow, origin: &CodeToolOrigin) -> bool {
    state
        .core_state
        .code_tools
        .scopes
        .get(&origin.execution_id)
        .is_some_and(|scope| scope.cancel_requested)
}
fn queue_completion(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    origin: CodeToolOrigin,
    outcome: crate::CodeToolInvokeActivityResult,
) {
    ctx.state_mut(|state| state.code_tools.completions.push((origin, outcome)));
}
async fn failed_result(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    origin: &CodeToolOrigin,
    status: ToolCallStatus,
    message: &str,
) -> crate::CodeToolInvokeActivityResult {
    let error_ref = tool_batches::put_boundary_error_blob(ctx, message).await;
    crate::CodeToolInvokeActivityResult::Completed {
        result: tool_batches::boundary_call_result(
            harness::code_tool_call_id(origin),
            status,
            error_ref,
        ),
    }
}

/// Poll SDK futures directly. Every readiness/preparation phase remains in the
/// same bounded window, so one blocked dependency cannot stall its siblings.
pub(super) async fn run_dispatch_loop(
    mut ctx: WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let activity_ctx = ctx.clone();
    let mut inflight: Vec<Inflight<'_>> = Vec::new();
    loop {
        while let Some((origin, exclusive)) = ctx.state(next_dispatch) {
            ctx.state_mut(|state| {
                state.code_tools.inflight.insert(key(&origin), exclusive);
            });
            if ctx.state(|state| scope_cancelled(state, &origin)) {
                queue_completion(
                    &ctx,
                    origin.clone(),
                    crate::CodeToolInvokeActivityResult::Completed {
                        result: canceled_result(harness::code_tool_call_id(&origin)),
                    },
                );
                continue;
            }
            let batch = ctx.state(|state| {
                harness::code_tool_request(
                    state.session_id.as_ref().expect("initialized session"),
                    &state.core_state,
                    &origin.execution_id,
                    &origin.request_id,
                )
            });
            let batch = match batch {
                Ok(request) => request,
                Err(error) => {
                    let result = failed_result(
                        &mut ctx,
                        &origin,
                        ToolCallStatus::Failed,
                        &error.to_string(),
                    )
                    .await;
                    queue_completion(&ctx, origin, result);
                    continue;
                }
            };
            let execution = ctx.state(|state| {
                let call = &batch.calls[0];
                if call.remote_mcp.is_some() {
                    harness::ToolExecutionSpec::new(
                        harness::ToolExecutionClass::RemoteInteractive,
                        false,
                    )
                } else {
                    state
                        .core_state
                        .tooling
                        .tools
                        .get(call.tool_id.as_ref().expect("admitted tool"))
                        .map(|tool| tool.execution)
                        .unwrap_or_default()
                }
            });
            let request = batch
                .call_request(0, execution)
                .expect("single code tool call");
            let future = if let Some(controls) = batch.promise_control_argument_request() {
                TaskFuture::PrepareControls(Box::pin(activity_ctx.execute_activity(
                    WorkflowActivities::tool_prepare_promise_controls,
                    crate::ToolPreparePromiseControlsActivityRequest { request: controls },
                    crate::boundary_error_blob_activity_options(),
                )))
            } else {
                start_invoke(&activity_ctx, &request)
            };
            inflight.push(Inflight {
                origin,
                request,
                future,
                cancel_sent: false,
                readiness_attempted: false,
            });
        }
        for task in &mut inflight {
            if !task.cancel_sent && ctx.state(|state| scope_cancelled(state, &task.origin)) {
                task.future.cancel();
                task.cancel_sent = true;
            }
        }
        let ready = {
            let uncanceled = inflight
                .iter()
                .filter(|task| !task.cancel_sent)
                .map(|task| task.origin.clone())
                .collect::<Vec<_>>();
            let wait = ctx.wait_for_state(move |state| {
                next_dispatch(state).is_some()
                    || uncanceled
                        .iter()
                        .any(|origin| scope_cancelled(state, origin))
            });
            let next = futures::future::poll_fn(|cx| {
                for (index, task) in inflight.iter_mut().enumerate() {
                    if let Poll::Ready(outcome) = task.future.poll(cx) {
                        return Poll::Ready((index, outcome));
                    }
                }
                Poll::Pending
            })
            .fuse();
            pin_mut!(next, wait);
            // A ready completion can schedule result materialization. Prefer
            // it consistently over new dispatch to preserve replay ordering.
            futures::select_biased! { ready = next => Some(ready), _ = wait => None }
        };
        let Some((index, outcome)) = ready else {
            continue;
        };
        let mut task = inflight.remove(index);
        let cancel = ctx.state(|state| scope_cancelled(state, &task.origin));
        let outcome = match outcome {
            TaskOutcome::Invoke(Ok(crate::CodeToolInvokeActivityResult::EnvironmentNotReady {
                environment_id,
            })) if !cancel && !task.readiness_attempted => {
                task.future = TaskFuture::Readiness(Box::pin(activity_ctx.execute_activity(
                    WorkflowActivities::await_environment_ready,
                    crate::AwaitEnvironmentReadyActivityRequest {
                        session_id: task.request.session_id.clone(),
                        environment_id,
                        environment_policy: task.request.environment_policy.clone(),
                    },
                    crate::environment_ready_activity_options(),
                )));
                task.readiness_attempted = true;
                inflight.push(task);
                continue;
            }
            TaskOutcome::Readiness(Ok(crate::AwaitEnvironmentReadyActivityResult::Ready))
                if !cancel =>
            {
                task.future = start_invoke(&activity_ctx, &task.request);
                inflight.push(task);
                continue;
            }
            TaskOutcome::PrepareControls(Ok(facts)) if !cancel => {
                let prepared = ctx.state(|state| {
                    harness::attach_promise_control_runtime(
                        &state.core_state,
                        task.request.clone().into_batch_request(),
                        facts,
                    )
                });
                match prepared {
                    Ok(batch) => {
                        task.request = batch
                            .call_request(0, task.request.execution)
                            .expect("single code tool call");
                        task.future = start_invoke(&activity_ctx, &task.request);
                        inflight.push(task);
                        continue;
                    }
                    Err(error) => {
                        failed_result(
                            &mut ctx,
                            &task.origin,
                            ToolCallStatus::Failed,
                            &error.to_string(),
                        )
                        .await
                    }
                }
            }
            // A completed effect remains authoritative even when it raced a close.
            TaskOutcome::Invoke(Ok(
                outcome @ crate::CodeToolInvokeActivityResult::Completed { .. },
            )) => outcome,
            TaskOutcome::Invoke(Ok(
                outcome @ crate::CodeToolInvokeActivityResult::Deferred { .. },
            )) => outcome,
            _ if cancel => crate::CodeToolInvokeActivityResult::Completed {
                result: canceled_result(task.request.call.call_id.clone()),
            },
            TaskOutcome::Invoke(Err(error))
            | TaskOutcome::Readiness(Err(error))
            | TaskOutcome::PrepareControls(Err(error)) => {
                failed_result(
                    &mut ctx,
                    &task.origin,
                    tool_batches::boundary_call_status(&error),
                    &error.to_string(),
                )
                .await
            }
            TaskOutcome::Readiness(Ok(result)) => {
                failed_result(
                    &mut ctx,
                    &task.origin,
                    ToolCallStatus::Failed,
                    &format!("environment unavailable: {result:?}"),
                )
                .await
            }
            TaskOutcome::Invoke(Ok(crate::CodeToolInvokeActivityResult::EnvironmentNotReady {
                ..
            })) => {
                failed_result(
                    &mut ctx,
                    &task.origin,
                    ToolCallStatus::Failed,
                    "environment remained unavailable after readiness wait",
                )
                .await
            }
            TaskOutcome::PrepareControls(Ok(_)) => {
                unreachable!("successful preparation handled above")
            }
        };
        queue_completion(&ctx, task.origin, outcome);
    }
}

fn canceled_result(call_id: harness::ToolCallId) -> harness::ToolInvocationResult {
    harness::ToolInvocationResult {
        call_id,
        status: ToolCallStatus::Cancelled,
        output_ref: None,
        error_ref: None,
        model_visible_context_entries: vec![],
        effects: vec![],
        attachments: vec![],
        duration_ms: None,
        output_bytes: None,
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_code_tool_catalog_excludes_the_parent_and_explicit_empty_grants_nothing() {
        let state = dispatcher_state();
        let parent = harness::ToolName::new("z-first");
        let tools = &state.core_state.tooling.tools;
        let selected = select_code_tools(tools, &parent, None).unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|tool| tool.name != parent));
        assert!(
            select_code_tools(tools, &parent, Some(&BTreeSet::new()))
                .unwrap()
                .is_empty()
        );
        for forbidden in [parent.clone(), harness::ToolName::new("not-granted")] {
            let error = select_code_tools(tools, &parent, Some(&[forbidden].into())).unwrap_err();
            assert_eq!(error.kind, Rejection::PermissionDenied);
        }
    }

    fn dispatcher_state() -> AgentSessionWorkflow {
        let mut state = AgentSessionWorkflow::default();
        let mut calls = BTreeMap::new();
        let mut bindings = BTreeMap::new();
        // Lexical order deliberately differs from admission order.
        for (request_id, base, parallelism) in [
            ("z-first", 1, ToolParallelism::ParallelSafe),
            ("a-second", 33, ToolParallelism::Exclusive),
            ("b-third", 65, ToolParallelism::ParallelSafe),
        ] {
            let tool_id = harness::ToolName::new(request_id);
            state.core_state.tooling.tools.insert(
                tool_id.clone(),
                harness::ToolSpec {
                    name: tool_id.clone(),
                    kind: harness::ToolKind::Builtin(Default::default()),
                    parallelism,
                    execution: Default::default(),
                },
            );
            bindings.insert(
                request_id.to_owned(),
                harness::CodeToolBinding {
                    tool_id: tool_id.clone(),
                    tool_name: tool_id.clone(),
                },
            );
            let origin = CodeToolOrigin {
                execution_id: "scope".into(),
                request_id: request_id.into(),
            };
            calls.insert(
                request_id.into(),
                harness::CodeToolCall {
                    call_id: harness::code_tool_call_id(&origin),
                    spec: harness::CodeToolCallSpec {
                        origin,
                        binding_id: request_id.into(),
                        tool_id: tool_id.clone(),
                        tool_name: tool_id,
                        arguments_ref: BlobRef::from_bytes(b"{}"),
                    },
                    promise_id_base: base,
                    status: CodeToolCallStatus::Pending,
                },
            );
        }
        state.core_state.code_tools.scopes.insert(
            "scope".into(),
            harness::CodeToolScope {
                toolset_revision: 0,
                spec: harness::CodeToolScopeSpec {
                    execution_id: "scope".into(),
                    parent_invocation_id: harness::WorkflowToolInvocationId::new(format!(
                        "wti:sha256:{}",
                        "0".repeat(64)
                    )),
                    bindings,
                    max_calls: 10,
                    max_in_flight: 3,
                },
                closed: false,
                cancel_requested: false,
                calls,
            },
        );
        state
    }

    #[test]
    fn dispatch_respects_admission_order_and_exclusive_barriers() {
        let mut state = dispatcher_state();
        let (first, exclusive) = next_dispatch(&state).unwrap();
        assert_eq!(first.request_id, "z-first");
        assert!(!exclusive);
        state.code_tools.inflight.insert(key(&first), false);
        assert!(
            next_dispatch(&state).is_none(),
            "exclusive call must wait; later parallel call must not jump it"
        );
        state
            .core_state
            .code_tools
            .scopes
            .get_mut("scope")
            .unwrap()
            .calls
            .get_mut("z-first")
            .unwrap()
            .status = CodeToolCallStatus::Completed {
            result: canceled_result(harness::code_tool_call_id(&first)).into(),
        };
        state.code_tools.inflight.clear();
        let (second, exclusive) = next_dispatch(&state).unwrap();
        assert_eq!(second.request_id, "a-second");
        assert!(exclusive);
        state.code_tools.inflight.insert(key(&second), true);
        assert!(next_dispatch(&state).is_none());
    }

    #[test]
    fn closed_scopes_allow_identical_delivery_but_reject_new_or_conflicting_calls() {
        let mut state = dispatcher_state();
        state
            .core_state
            .code_tools
            .scopes
            .get_mut("scope")
            .unwrap()
            .closed = true;
        let request = crate::InvokeCodeToolRequest {
            execution_id: "scope".into(),
            request_id: "z-first".into(),
            binding_id: "z-first".into(),
            arguments_ref: BlobRef::from_bytes(b"{}"),
        };
        assert!(invocation_command(&state, request.clone()).is_ok());
        assert_eq!(
            invocation_command(
                &state,
                crate::InvokeCodeToolRequest {
                    arguments_ref: BlobRef::from_bytes(b"changed"),
                    ..request.clone()
                }
            )
            .unwrap_err()
            .kind,
            Rejection::Conflict
        );
        assert_eq!(
            invocation_command(
                &state,
                crate::InvokeCodeToolRequest {
                    request_id: "new".into(),
                    ..request
                }
            )
            .unwrap_err()
            .kind,
            Rejection::ScopeClosed
        );
    }

    #[test]
    fn active_transport_and_unsettled_calls_block_rollover_and_parent_resume() {
        let mut state = dispatcher_state();
        assert!(blocks_parent_resume(&state));
        state.code_tools.waiters = 1;
        assert!(!is_quiescent(&state));
        state.code_tools.waiters = 0;
        state
            .code_tools
            .inflight
            .insert(("scope".into(), "z-first".into()), false);
        assert!(!is_quiescent(&state));
        state.code_tools.inflight.clear();
        assert!(
            is_quiescent(&state),
            "pending durable calls can be dispatched after rehydration"
        );
    }
    #[test]
    fn last_update_waiter_wakes_closed_session_completion() {
        let mut state = AgentSessionWorkflow {
            initialized: true,
            ready: true,
            ..Default::default()
        };
        state.core_state.lifecycle.status = CoreAgentStatus::Closed;
        state.code_tools.waiters = 1;
        assert!(!wait_loop::workflow_state_is_closed_and_quiescent(&state));
        assert!(!wait_loop::workflow_state_has_immediate_work(&state));

        // The handler consumes its final outcome and returns without another
        // append or timer. This state change must wake the main session loop.
        state.code_tools.waiters -= 1;
        assert!(wait_loop::workflow_state_is_closed_and_quiescent(&state));
        assert!(wait_loop::workflow_state_has_immediate_work(&state));
    }
}
