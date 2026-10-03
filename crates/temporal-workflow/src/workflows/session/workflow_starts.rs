use super::*;

const MAX_WORKFLOW_START_ATTEMPTS: u32 = 5;
const WORKFLOW_START_RETRY_BACKOFF_MS: u64 = 2_000;

/// One start-on-call intent that still needs its deterministic execution
/// issued. Pending start work is recomputed from durable state
/// (`start_requests` minus terminal failures minus resolved promise sets),
/// so replay and continue-as-new rebuild it without transport bookkeeping;
/// the deterministic execution id makes re-issuing safe.
struct StartCandidate {
    invocation: harness::WorkflowToolInvocation,
    execution_id: String,
    start: harness::WorkflowStartRef,
}

fn start_candidates(state: &AgentSessionWorkflow) -> Vec<StartCandidate> {
    state
        .core_state
        .workflow_tools
        .start_requests
        .values()
        .filter_map(|invocation| {
            if state
                .core_state
                .workflow_tools
                .start_failures
                .contains_key(&invocation.invocation_id)
            {
                return None;
            }
            if state
                .confirmed_workflow_starts
                .contains(invocation.invocation_id.as_str())
            {
                return None;
            }
            let has_pending_promise = invocation
                .completion_promises
                .iter()
                .flat_map(|promises| promises.values())
                .any(|promise_id| {
                    state
                        .core_state
                        .promises
                        .promises
                        .get(promise_id)
                        .is_some_and(|promise| promise.status == harness::PromiseStatus::Pending)
                });
            if !has_pending_promise {
                return None;
            }
            let binding = state
                .core_state
                .workflow_tools
                .bindings
                .get(&invocation.tool_id)?;
            let harness::WorkflowToolTarget::Start { start } = &binding.target else {
                return None;
            };
            Some(StartCandidate {
                invocation: invocation.clone(),
                execution_id: harness::workflow_tool_execution_id(
                    &invocation.invocation_id,
                    &start.recipe_fingerprint,
                ),
                start: start.clone(),
            })
        })
        .collect()
}

pub(super) fn has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    start_candidates(state).iter().any(|candidate| {
        !state
            .workflow_start_backoffs
            .contains_key(candidate.invocation.invocation_id.as_str())
    })
}

pub(super) fn nearest_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    let candidates = start_candidates(state);
    candidates
        .iter()
        .filter_map(|candidate| {
            state
                .workflow_start_backoffs
                .get(candidate.invocation.invocation_id.as_str())
                .map(|(_, next_at_ms)| *next_at_ms)
        })
        .min()
}

/// Issue every due deterministic start. `AlreadyStarted` is success for the
/// exact identity; transient failures retry with bounded deterministic
/// backoff; exhausted or terminal failures append `StartFailed` and fail the
/// invocation's still-pending keyed promises atomically.
pub(super) async fn process_pending_starts(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let due = ctx.state(|state| {
        start_candidates(state)
            .into_iter()
            .filter(|candidate| {
                state
                    .workflow_start_backoffs
                    .get(candidate.invocation.invocation_id.as_str())
                    .is_none_or(|(_, next_at_ms)| *next_at_ms <= now)
            })
            .map(|candidate| {
                (
                    candidate.invocation.clone(),
                    candidate.execution_id,
                    candidate.start,
                )
            })
            .collect::<Vec<_>>()
    });
    for (invocation, execution_id, start) in due {
        let invocation_key = invocation.invocation_id.as_str().to_owned();
        let result = ctx
            .start_activity(
                WorkflowActivities::start_workflow_tool_execution,
                crate::WorkflowToolStartActivityRequest {
                    execution_id,
                    recipe_format: start.recipe_format,
                    recipe_revision: start.revision,
                    recipe_ref: start.recipe_ref,
                    recipe_fingerprint: start.recipe_fingerprint,
                    holder_workflow_id: ctx.workflow_id().to_owned(),
                    invocation: invocation.clone(),
                },
                activity_options(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        match result {
            crate::WorkflowToolStartActivityResult::Started => {
                ctx.state_mut(|state| {
                    state.workflow_start_backoffs.remove(&invocation_key);
                    state.confirmed_workflow_starts.insert(invocation_key);
                });
            }
            crate::WorkflowToolStartActivityResult::FailedRetryable { message } => {
                let attempts = ctx.state(|state| {
                    state
                        .workflow_start_backoffs
                        .get(&invocation_key)
                        .map_or(0, |(attempts, _)| *attempts)
                }) + 1;
                if attempts >= MAX_WORKFLOW_START_ATTEMPTS {
                    terminal_start_failure(ctx, &invocation, attempts, message).await?;
                    ctx.state_mut(|state| {
                        state.workflow_start_backoffs.remove(&invocation_key);
                    });
                } else {
                    let next_at_ms = now + WORKFLOW_START_RETRY_BACKOFF_MS * u64::from(attempts);
                    ctx.state_mut(|state| {
                        state
                            .workflow_start_backoffs
                            .insert(invocation_key.clone(), (attempts, next_at_ms));
                    });
                }
            }
            crate::WorkflowToolStartActivityResult::FailedTerminal { message } => {
                terminal_start_failure(ctx, &invocation, 1, message).await?;
                ctx.state_mut(|state| {
                    state.workflow_start_backoffs.remove(&invocation_key);
                });
            }
        }
    }
    Ok(())
}

async fn terminal_start_failure(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    invocation: &harness::WorkflowToolInvocation,
    attempts: u32,
    message: String,
) -> anyhow::Result<()> {
    let detail = format!(
        "workflow tool start intent {} could not start its execution after {attempts} attempts: {message}",
        invocation.invocation_id
    );
    let error_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: detail.into_bytes(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut drive = drive_from_state(ctx)?;
    match admit_and_append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::FailWorkflowToolStart {
            invocation_id: invocation.invocation_id.clone(),
            error_ref,
        },
        None,
    )
    .await?
    {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            ctx.state_mut(|state| state.admission_failures.push(failure));
            Ok(())
        }
    }
}

/// When the last still-pending keyed promise of a session-owned started
/// execution reaches a terminal state — by resolution, failure, or
/// cancellation — request cancellation of the exact execution. A normally
/// completed execution treats the cancel as a no-op; the plugin workflow
/// owns cleanup of its activities and external resources.
pub(super) async fn process_execution_cancels(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let due = ctx.state(|state| {
        state
            .core_state
            .workflow_tools
            .start_requests
            .values()
            .filter_map(|invocation| {
                let promises = invocation.completion_promises.as_ref()?;
                let all_terminal = promises.values().all(|promise_id| {
                    state
                        .core_state
                        .promises
                        .promises
                        .get(promise_id)
                        .is_some_and(|promise| promise.status.is_terminal())
                });
                if !all_terminal {
                    return None;
                }
                let binding = state
                    .core_state
                    .workflow_tools
                    .bindings
                    .get(&invocation.tool_id)?;
                let harness::WorkflowToolTarget::Start { start } = &binding.target else {
                    return None;
                };
                let execution_id = harness::workflow_tool_execution_id(
                    &invocation.invocation_id,
                    &start.recipe_fingerprint,
                );
                (!state.cancelled_workflow_executions.contains(&execution_id))
                    .then_some(execution_id)
            })
            .collect::<Vec<_>>()
    });
    for execution_id in due {
        if let Err(error) = ctx
            .start_activity(
                WorkflowActivities::cancel_workflow_tool_execution,
                crate::WorkflowToolExecutionCancelRequest {
                    execution_id: execution_id.clone(),
                },
                activity_options(),
            )
            .await
        {
            // Best-effort: record and move on rather than wedging the loop.
            ctx.state_mut(|state| {
                state.last_error = Some(format!(
                    "cancel workflow tool execution {execution_id}: {error}"
                ));
            });
        }
        ctx.state_mut(|state| {
            state.cancelled_workflow_executions.insert(execution_id);
        });
    }
    Ok(())
}
