//! Per-call tool batch execution.
//!
//! Ordinary batches schedule one Temporal activity per executable call and
//! resume the drive progressively: each terminal call result appends durably
//! on its own, so a slow or failed call never restarts a completed sibling.
//! Batches that need batch-level orchestration (an `await` call or admitted
//! workflow-tool calls) still execute as one unit behind the same
//! progressive-completion engine contract.

use std::pin::Pin;
use std::task::{Context, Poll};

use engine::{ToolCallStatus, ToolExecutionSpec, ToolInvocationResult, ToolName, ToolParallelism};
use futures::FutureExt;
use futures::future::poll_fn;
use temporalio_sdk::{ActivityExecutionError, CancellableFuture};

use crate::{
    AwaitEnvironmentReadyActivityRequest, AwaitEnvironmentReadyActivityResult,
    MAX_CONCURRENT_TOOL_CALLS_PER_BATCH, ToolInvokeCallActivityRequest,
    ToolInvokeCallActivityResult, boundary_error_blob_activity_options,
    environment_ready_activity_options, tool_call_activity_options,
};

use super::*;

/// Longest boundary-failure message recorded for the model; put_blob inputs
/// stay bounded even for pathological error chains.
const MAX_BOUNDARY_ERROR_BYTES: usize = 16 * 1024;

pub(super) async fn invoke_tool_batch(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: ToolInvocationBatchRequest,
) -> anyhow::Result<CoreAgentAction> {
    if batch_requires_unit_execution(&request) {
        return invoke_tool_batch_as_unit(ctx, drive, request).await;
    }
    for group in execution_groups(drive.state(), &request) {
        if !control::tool_batch_still_wanted(drive.state(), request.run_id, request.batch_id) {
            // A cancel landed while an earlier group ran; the engine has
            // resolved the rest of the batch itself.
            break;
        }
        execute_call_group(ctx, drive, &request, group).await?;
    }
    drive
        .next_action_unbounded(workflow_time_ms(ctx))
        .map_err(Into::into)
}

/// `await` defers the whole batch and admitted workflow-tool calls share
/// batch-scoped emission ordering; both stay on the batch-unit activity.
fn batch_requires_unit_execution(request: &ToolInvocationBatchRequest) -> bool {
    request.calls.iter().any(|call| {
        call.workflow_tool.is_some()
            || call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "concurrency.await")
    })
}

async fn invoke_tool_batch_as_unit(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: ToolInvocationBatchRequest,
) -> anyhow::Result<CoreAgentAction> {
    let run_id = request.run_id;
    let batch_id = request.batch_id;
    let activity_ctx = ctx.clone();
    let activity = activity_ctx.start_activity(
        WorkflowActivities::tool_invoke_batch,
        ToolInvokeBatchActivityRequest {
            request: request.clone(),
        },
        crate::tool_batch_activity_options(),
    );
    let outcome = match control::race_activity_with_admissions(ctx, drive, activity, |state| {
        control::tool_batch_still_wanted(state, run_id, batch_id)
    })
    .await?
    {
        control::Raced::Completed(outcome) => outcome,
        control::Raced::Preempted => {
            return drive
                .next_action_unbounded(workflow_time_ms(ctx))
                .map_err(Into::into);
        }
    };
    match outcome {
        Ok(outcome) => drive
            .resume_tool_batch_outcome(outcome, workflow_time_ms(ctx))
            .map_err(Into::into),
        Err(error) => {
            let status = boundary_call_status(&error);
            let error_ref = put_boundary_error_blob(ctx, &format!("{error}")).await;
            let results = request
                .calls
                .iter()
                .map(|call| boundary_call_result(call.call_id.clone(), status, error_ref.clone()))
                .collect();
            drive
                .resume_tool_batch(
                    engine::ToolInvocationBatchResult {
                        run_id: request.run_id,
                        turn_id: request.turn_id,
                        batch_id: request.batch_id,
                        results,
                    },
                    workflow_time_ms(ctx),
                )
                .map_err(Into::into)
        }
    }
}

/// Execution order over call indices: consecutive parallel-safe calls run
/// concurrently; an exclusive call runs alone, in original batch order.
fn execution_groups(
    state: &CoreAgentState,
    request: &ToolInvocationBatchRequest,
) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut last_parallel_safe = false;
    for (index, call) in request.calls.iter().enumerate() {
        let parallel_safe =
            call_parallelism(state, call.tool_id.as_ref()) == ToolParallelism::ParallelSafe;
        match groups.last_mut() {
            Some(group) if parallel_safe && last_parallel_safe => group.push(index),
            _ => groups.push(vec![index]),
        }
        last_parallel_safe = parallel_safe;
    }
    groups
}

type CallActivityOutcome = Result<ToolInvokeCallActivityResult, ActivityExecutionError>;

/// One in-flight call activity. The future is the SDK's cancellable activity
/// future itself (no wrapper), so a preempting cancel can reach it; it is
/// polled with the caller's context directly — combinators with internal
/// waker machinery (e.g. `FuturesUnordered`) trip the SDK's nondeterminism
/// detector ([TMPRL1100]) and must not be used inside workflow code.
struct InflightCall<'a> {
    index: usize,
    activity: Pin<Box<dyn CancellableFuture<CallActivityOutcome> + 'a>>,
}

/// Resolve the first ready in-flight call, removing it from `inflight`.
async fn first_ready_call(inflight: &mut Vec<InflightCall<'_>>) -> (usize, CallActivityOutcome) {
    poll_fn(|cx: &mut Context<'_>| {
        for position in 0..inflight.len() {
            if let Poll::Ready(outcome) = inflight[position].activity.as_mut().poll(cx) {
                let call = inflight.remove(position);
                return Poll::Ready((call.index, outcome));
            }
        }
        Poll::Pending
    })
    .await
}

/// Cancel every in-flight call (`TryCancel`) and let the futures resolve;
/// their results are discarded — the engine already recorded the calls as
/// cancelled.
async fn abandon_inflight_calls(inflight: Vec<InflightCall<'_>>) {
    for call in inflight {
        call.activity.as_ref().get_ref().cancel();
        let _ = call.activity.await;
    }
}

async fn execute_call_group(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: &ToolInvocationBatchRequest,
    group: Vec<usize>,
) -> anyhow::Result<()> {
    let activity_ctx = ctx.clone();
    let mut pending = group.into_iter();
    let mut inflight: Vec<InflightCall<'_>> = Vec::new();
    loop {
        // Topped-up window: at most MAX_CONCURRENT_TOOL_CALLS_PER_BATCH
        // activities execute at once, so one model turn cannot fan out
        // unbounded concurrent work.
        while inflight.len() < MAX_CONCURRENT_TOOL_CALLS_PER_BATCH {
            let Some(index) = pending.next() else { break };
            inflight.push(start_call_activity(
                &activity_ctx,
                drive.state(),
                request,
                index,
            ));
        }
        if inflight.is_empty() {
            return Ok(());
        }
        // Race the window against client admissions: a steer or a
        // queued run is admitted and execution continues; a cancel that
        // ends this batch abandons every in-flight call.
        let ready = {
            let wait = ctx.wait_condition(admissions::has_admissible_admissions);
            let next = first_ready_call(&mut inflight).fuse();
            pin_mut!(wait, next);
            select! {
                ready = next => Some(ready),
                _ = wait => None,
            }
        };
        match ready {
            Some((index, outcome)) => {
                resume_call(ctx, drive, request, index, outcome).await?;
            }
            None => {
                admissions::drain_pending_admissions(ctx, drive).await?;
                if !control::tool_batch_still_wanted(
                    drive.state(),
                    request.run_id,
                    request.batch_id,
                ) {
                    abandon_inflight_calls(inflight).await;
                    return Ok(());
                }
            }
        }
    }
}

fn start_call_activity<'a>(
    activity_ctx: &'a WorkflowContext<AgentSessionWorkflow>,
    state: &CoreAgentState,
    request: &ToolInvocationBatchRequest,
    index: usize,
) -> InflightCall<'a> {
    InflightCall {
        index,
        activity: Box::pin(call_activity(activity_ctx, state, request, index)),
    }
}

fn call_activity<'a>(
    activity_ctx: &'a WorkflowContext<AgentSessionWorkflow>,
    state: &CoreAgentState,
    request: &ToolInvocationBatchRequest,
    index: usize,
) -> impl CancellableFuture<CallActivityOutcome> + use<'a> {
    let call = &request.calls[index];
    // The engine materialized the native MCP routing facts on the call when
    // it built this dispatch; the workflow only selects the execution class.
    let execution = if call.remote_mcp.is_some() {
        ToolExecutionSpec::new(engine::ToolExecutionClass::RemoteInteractive, false)
    } else {
        call_execution_spec(state, call.tool_id.as_ref())
    };
    let call_request = request
        .call_request(index, execution)
        .expect("group indices come from this batch request");
    activity_ctx.start_activity(
        WorkflowActivities::tool_invoke_call,
        ToolInvokeCallActivityRequest {
            request: call_request,
        },
        tool_call_activity_options(execution),
    )
}

/// Accept one call outcome: an activity failure (deadline, exhausted retries,
/// infrastructure) becomes an ordinary terminal failed call result, a
/// cancelled activity records a terminal cancelled result, and a result whose
/// call id does not match the scheduled call is rejected as a boundary
/// failure. A call that did not execute because its active environment is
/// still provisioning waits for readiness in a dedicated heartbeated activity
/// and is re-dispatched once. The drive then appends the durable
/// per-call completion.
async fn resume_call(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: &ToolInvocationBatchRequest,
    index: usize,
    outcome: Result<ToolInvokeCallActivityResult, ActivityExecutionError>,
) -> anyhow::Result<()> {
    if let Ok(ToolInvokeCallActivityResult::NeedsApproval { subject }) = &outcome {
        let action = drive.request_native_mcp_approvals(
            request.batch_id,
            vec![engine::NativeMcpApprovalRequest {
                call_id: request.calls[index].call_id.clone(),
                subject: subject.clone(),
            }],
            workflow_time_ms(ctx),
        )?;
        return match action {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => drive::append_events(ctx, drive, expected_head, events)
                .await
                .map(|_| ()),
            other => {
                anyhow::bail!("native MCP approval request emitted unexpected action: {other:?}")
            }
        };
    }
    let outcome = match outcome {
        Ok(ToolInvokeCallActivityResult::EnvironmentNotReady { environment_id }) => {
            match await_environment_then_redispatch(ctx, drive, request, index, environment_id)
                .await?
            {
                Some(outcome) => outcome,
                // Preempted by a cancel: the engine recorded the call as
                // cancelled; nothing to resume.
                None => return Ok(()),
            }
        }
        Ok(ToolInvokeCallActivityResult::Completed { result }) => CallOutcome::Completed(result),
        Ok(ToolInvokeCallActivityResult::NeedsApproval { .. }) => {
            unreachable!("handled above")
        }
        Err(error) => CallOutcome::ActivityFailed(Box::new(error)),
    };
    let scheduled_call_id = &request.calls[index].call_id;
    let result = match outcome {
        CallOutcome::Completed(result) if result.call_id == *scheduled_call_id => result,
        CallOutcome::Completed(result) => {
            let error_ref = put_boundary_error_blob(
                ctx,
                &format!(
                    "tool runtime returned a result for call {} instead of the scheduled call {}",
                    result.call_id, scheduled_call_id
                ),
            )
            .await;
            boundary_call_result(scheduled_call_id.clone(), ToolCallStatus::Failed, error_ref)
        }
        CallOutcome::ActivityFailed(error) => {
            let status = boundary_call_status(&error);
            let error_ref = put_boundary_error_blob(ctx, &format!("{error}")).await;
            boundary_call_result(scheduled_call_id.clone(), status, error_ref)
        }
        CallOutcome::EnvironmentUnusable(message) => {
            let error_ref = put_boundary_error_blob(ctx, &message).await;
            boundary_call_result(scheduled_call_id.clone(), ToolCallStatus::Failed, error_ref)
        }
    };
    let action = drive.resume_tool_call(request.batch_id, result, workflow_time_ms(ctx))?;
    match action {
        CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } => {
            drive::append_events(ctx, drive, expected_head, events).await?;
            Ok(())
        }
        other => anyhow::bail!("tool call resume emitted unexpected action: {other:?}"),
    }
}

/// One call's outcome after the optional readiness wait.
enum CallOutcome {
    Completed(ToolInvocationResult),
    ActivityFailed(Box<ActivityExecutionError>),
    /// The active environment failed, closed, or stayed unready past the
    /// bounded wait; recorded as a terminal failed call with this message.
    EnvironmentUnusable(String),
}

/// Wait for the session's active environment, then run the call once more
/// with its ordinary options. The wait is its own activity so tool classes
/// keep their tight deadlines; the fast path never reaches this function.
/// Both steps race client admissions; `None` means a cancel made the call
/// obsolete.
async fn await_environment_then_redispatch(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: &ToolInvocationBatchRequest,
    index: usize,
    environment_id: String,
) -> anyhow::Result<Option<CallOutcome>> {
    let run_id = request.run_id;
    let batch_id = request.batch_id;
    let still_wanted =
        |state: &CoreAgentState| control::tool_batch_still_wanted(state, run_id, batch_id);
    let activity_ctx = ctx.clone();
    let readiness = activity_ctx.start_activity(
        WorkflowActivities::await_environment_ready,
        AwaitEnvironmentReadyActivityRequest {
            session_id: request.session_id.clone(),
            environment_id: environment_id.clone(),
            environment_policy: request.environment_policy.clone(),
        },
        environment_ready_activity_options(),
    );
    let readiness =
        match control::race_activity_with_admissions(ctx, drive, readiness, still_wanted).await? {
            control::Raced::Completed(readiness) => readiness,
            control::Raced::Preempted => return Ok(None),
        };
    let outcome = match readiness {
        Ok(AwaitEnvironmentReadyActivityResult::Ready) => {
            let call = call_activity(&activity_ctx, drive.state(), request, index);
            let outcome =
                match control::race_activity_with_admissions(ctx, drive, call, still_wanted).await?
                {
                    control::Raced::Completed(outcome) => outcome,
                    control::Raced::Preempted => return Ok(None),
                };
            match outcome {
                Ok(ToolInvokeCallActivityResult::Completed { result }) => {
                    CallOutcome::Completed(result)
                }
                // The environment regressed between the wait and the
                // re-dispatch. Do not loop: report it.
                Ok(ToolInvokeCallActivityResult::EnvironmentNotReady { environment_id }) => {
                    CallOutcome::EnvironmentUnusable(format!(
                        "active environment {environment_id} was still not ready after the readiness wait"
                    ))
                }
                Ok(ToolInvokeCallActivityResult::NeedsApproval { .. }) => {
                    CallOutcome::EnvironmentUnusable(
                        "unexpected native MCP approval after environment readiness wait"
                            .to_owned(),
                    )
                }
                Err(error) => CallOutcome::ActivityFailed(Box::new(error)),
            }
        }
        Ok(AwaitEnvironmentReadyActivityResult::Failed { message }) => {
            CallOutcome::EnvironmentUnusable(format!(
                "active environment {environment_id} cannot serve this call: {message}"
            ))
        }
        Ok(AwaitEnvironmentReadyActivityResult::TimedOut { last_status }) => {
            CallOutcome::EnvironmentUnusable(format!(
                "active environment {environment_id} was still {last_status} after waiting {}s",
                crate::ENVIRONMENT_READY_WAIT.as_secs()
            ))
        }
        Err(error) => CallOutcome::ActivityFailed(Box::new(error)),
    };
    Ok(Some(outcome))
}

/// Cancellation remains cancellation: a cancelled activity records a terminal
/// cancelled call result instead of an ordinary failure.
fn boundary_call_status(error: &ActivityExecutionError) -> ToolCallStatus {
    match error {
        ActivityExecutionError::Cancelled(_) => ToolCallStatus::Cancelled,
        _ => ToolCallStatus::Failed,
    }
}

fn call_execution_spec(state: &CoreAgentState, tool_name: Option<&ToolName>) -> ToolExecutionSpec {
    tool_name
        .and_then(|id| state.tooling.tools.get(id))
        .map(|tool| tool.execution)
        .unwrap_or_default()
}

fn call_parallelism(state: &CoreAgentState, tool_name: Option<&ToolName>) -> ToolParallelism {
    tool_name
        .and_then(|id| state.tooling.tools.get(id))
        .map(|tool| tool.parallelism)
        .unwrap_or(ToolParallelism::Exclusive)
}

/// Materialize the boundary error text with bounded attempts. This path must
/// never reintroduce unlimited retries: when the bounded put fails, fall back
/// to the engine's well-known boundary-failure blob, which every runtime
/// guarantees exists.
async fn put_boundary_error_blob(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    error: &str,
) -> BlobRef {
    let mut message = format!("tool call failed at the runtime boundary: {error}");
    if message.len() > MAX_BOUNDARY_ERROR_BYTES {
        let mut end = MAX_BOUNDARY_ERROR_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    ctx.start_activity(
        WorkflowActivities::put_blob,
        PutBlobRequest {
            bytes: message.into_bytes(),
        },
        boundary_error_blob_activity_options(),
    )
    .await
    .unwrap_or_else(|_| engine::tool_runtime_boundary_failure_ref())
}

fn boundary_call_result(
    call_id: engine::ToolCallId,
    status: ToolCallStatus,
    error_ref: BlobRef,
) -> ToolInvocationResult {
    ToolInvocationResult {
        duration_ms: None,
        output_bytes: None,
        truncated: false,
        call_id: call_id.clone(),
        status,
        output_ref: None,
        model_visible_context_entries: vec![ToolInvocationResult::tool_result_context_entry(
            &call_id,
            status,
            error_ref.clone(),
        )],
        error_ref: Some(error_ref),
        effects: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use engine::{BlobRef, ToolCallId, ToolInvocationRequest, ToolSpec};

    use super::*;

    fn call(name: &str, id: &str) -> ToolInvocationRequest {
        ToolInvocationRequest {
            builtin: None,
            call_id: ToolCallId::new(id),
            tool_id: Some(ToolName::new(if name == "await" {
                "concurrency.await"
            } else {
                name
            })),
            tool_name: ToolName::new(name),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            workflow_tool: None,
            promise_control: None,
            remote_mcp: None,
        }
    }

    fn request_with(calls: Vec<ToolInvocationRequest>) -> ToolInvocationBatchRequest {
        ToolInvocationBatchRequest {
            vfs_working_directory: None,
            session_id: SessionId::new("session-a"),
            run_id: engine::RunId::new(1),
            turn_id: engine::TurnId::new(1),
            batch_id: engine::ToolBatchId::new(1),
            promise_id_base: 1,
            workspace_links: Vec::new(),
            active_environment_id: None,
            environment_policy: None,
            subagents_policy: None,
            calls,
        }
    }

    fn state_with_tools(specs: Vec<(&str, ToolParallelism)>) -> CoreAgentState {
        let mut state = CoreAgentState::default();
        for (name, parallelism) in specs {
            state.tooling.tools.insert(
                ToolName::new(name),
                ToolSpec {
                    name: ToolName::new(name),
                    kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                        description_ref: None,
                        input_schema_ref: BlobRef::from_bytes(b"{}"),
                        output_schema_ref: None,
                        strict: None,
                        provider_options_ref: None,
                    }),
                    parallelism,
                    execution: ToolExecutionSpec::default(),
                },
            );
        }
        state
    }

    #[test]
    fn await_and_workflow_tool_batches_execute_as_unit() {
        assert!(batch_requires_unit_execution(&request_with(vec![
            call("read_file", "call_read"),
            call("await", "call_await"),
        ])));
        assert!(!batch_requires_unit_execution(&request_with(vec![call(
            "read_file",
            "call_read"
        )])));
    }

    #[test]
    fn execution_groups_run_parallel_safe_runs_together_and_exclusive_alone() {
        let state = state_with_tools(vec![
            ("read_file", ToolParallelism::ParallelSafe),
            ("grep", ToolParallelism::ParallelSafe),
            ("write_file", ToolParallelism::Exclusive),
        ]);
        let request = request_with(vec![
            call("read_file", "call_1"),
            call("grep", "call_2"),
            call("write_file", "call_3"),
            call("read_file", "call_4"),
        ]);

        assert_eq!(
            execution_groups(&state, &request),
            vec![vec![0, 1], vec![2], vec![3]]
        );
    }

    #[test]
    fn unknown_tools_default_to_exclusive_execution() {
        let state = state_with_tools(Vec::new());
        let request = request_with(vec![call("mystery", "call_1"), call("mystery", "call_2")]);

        assert_eq!(execution_groups(&state, &request), vec![vec![0], vec![1]]);
    }
}
