use super::*;

pub(super) fn wait_directive_for_event(
    event: &CoreAgentEvent,
) -> anyhow::Result<Option<DeferredWait>> {
    let CoreAgentEvent::Tool(ToolEvent::BatchDeferred {
        run_id,
        turn_id,
        batch_id,
        resume_directive,
    }) = event
    else {
        return Ok(None);
    };
    if resume_directive.api_kind != FLEET_AGENT_WAIT_DIRECTIVE_KIND {
        return Ok(None);
    }
    let directive: AgentWaitDirective = serde_json::from_value(resume_directive.body.clone())
        .map_err(|error| anyhow::anyhow!("invalid agent_wait resume directive: {error}"))?;
    Ok(Some(DeferredWait {
        run_id: *run_id,
        turn_id: *turn_id,
        batch_id: *batch_id,
        directive,
    }))
}

#[derive(Clone, Debug)]
pub(super) struct DeferredWait {
    run_id: engine::RunId,
    turn_id: engine::TurnId,
    batch_id: engine::ToolBatchId,
    directive: AgentWaitDirective,
}

pub(super) fn mark_wait_terminal_arrival(
    wait: &mut ActiveWaitRecord,
    notification: &RunTerminalNotification,
) {
    let Some(subscription) = wait.subscriptions.iter().find(|subscription| {
        subscription.subscription.correlation_token == notification.correlation_token
    }) else {
        return;
    };
    let target_session_id = subscription.target_session_id.as_str();
    let run_id = api_run_id(notification.run_id);
    let Some(result) = wait
        .results
        .iter_mut()
        .find(|result| result.target_session_id == target_session_id && result.run_id == run_id)
    else {
        return;
    };
    if result.status == AgentWaitHandleStatus::Terminal {
        return;
    }
    result.status = AgentWaitHandleStatus::Terminal;
    result.run = Some(AgentWaitRunResult {
        status: run_status_name(notification.status),
        output_ref: notification.output_ref.clone(),
        failure_message_ref: notification.failure_message_ref.clone(),
    });
    result.error = None;
}

fn mark_wait_handle_error(
    wait: &mut ActiveWaitRecord,
    target_session_id: &SessionId,
    run_id: engine::RunId,
    error: impl Into<String>,
) {
    let api_run_id = api_run_id(run_id);
    let Some(result) = wait.results.iter_mut().find(|result| {
        result.target_session_id == target_session_id.as_str() && result.run_id == api_run_id
    }) else {
        return;
    };
    if result.status == AgentWaitHandleStatus::Terminal {
        return;
    }
    result.status = AgentWaitHandleStatus::Error;
    result.run = None;
    result.error = Some(error.into());
}

fn run_status_name(status: RunStatus) -> String {
    match status {
        RunStatus::Active => "running",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
    .to_owned()
}

pub(super) fn api_run_id(run_id: engine::RunId) -> String {
    format!("run_{}", run_id.as_u64())
}

fn wait_subscription_id(
    batch_id: engine::ToolBatchId,
    target_session_id: &SessionId,
    run_id: engine::RunId,
) -> String {
    format!(
        "fleet_wait_{}_{}_{}",
        batch_id.as_u64(),
        target_session_id.as_str(),
        run_id.as_u64()
    )
}

fn wait_correlation_token(
    batch_id: engine::ToolBatchId,
    target_session_id: &SessionId,
    run_id: engine::RunId,
) -> String {
    format!(
        "fleet_wait:{}:{}:{}",
        batch_id.as_u64(),
        target_session_id.as_str(),
        run_id.as_u64()
    )
}

async fn wait_model_visible_context_entries(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    call_id: &engine::ToolCallId,
    output: &AgentWaitOutput,
) -> anyhow::Result<Vec<ContextEntryInput>> {
    let summary_ref = put_blob_bytes(ctx, wait_model_visible_summary(output).into_bytes()).await?;
    let mut entries = vec![ToolInvocationResult::tool_result_context_entry(
        call_id,
        ToolCallStatus::Succeeded,
        summary_ref,
    )];
    for result in output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
    {
        append_wait_terminal_visible_context_entries(ctx, &mut entries, result).await?;
    }
    Ok(entries)
}

fn wait_model_visible_summary(output: &AgentWaitOutput) -> String {
    let terminal = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
        .count();
    let pending = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Pending)
        .count();
    let errors = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Error)
        .count();
    format!(
        "agent_wait resolved with outcome {} (terminal: {terminal}, pending: {pending}, errors: {errors}).",
        wait_outcome_name(output.outcome)
    )
}

async fn append_wait_terminal_visible_context_entries(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    entries: &mut Vec<ContextEntryInput>,
    result: &AgentWaitHandleResult,
) -> anyhow::Result<()> {
    let mut prefix = String::from("Agent run final output");
    prefix.push_str("\ntarget_session_id: ");
    prefix.push_str(&result.target_session_id);
    prefix.push_str("\nrun_id: ");
    prefix.push_str(&result.run_id);

    let Some(run) = result.run.as_ref() else {
        prefix.push_str("\nstatus: terminal\n\nNo run details were recorded.");
        append_wait_text_message(ctx, entries, prefix).await?;
        return Ok(());
    };
    prefix.push_str("\nstatus: ");
    prefix.push_str(&run.status);
    append_wait_text_message(ctx, entries, prefix).await?;

    if let Some(output_ref) = run.output_ref.as_ref() {
        entries.push(wait_user_message(
            output_ref.clone(),
            Some("Agent run final output"),
        ));
    } else if let Some(failure_ref) = run.failure_message_ref.as_ref() {
        entries.push(wait_user_message(
            failure_ref.clone(),
            Some("Agent run failure message"),
        ));
    } else {
        append_wait_text_message(ctx, entries, "No final output was recorded.").await?;
    }
    Ok(())
}

async fn append_wait_text_message(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    entries: &mut Vec<ContextEntryInput>,
    text: impl Into<String>,
) -> anyhow::Result<()> {
    let text = text.into();
    let content_ref = put_blob_bytes(ctx, text.clone().into_bytes()).await?;
    entries.push(wait_user_message(content_ref, Some(&text)));
    Ok(())
}

fn wait_user_message(content_ref: BlobRef, preview: Option<&str>) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: None,
        preview: preview.map(|value| value.chars().take(160).collect()),
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

async fn put_blob_bytes(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    bytes: Vec<u8>,
) -> anyhow::Result<BlobRef> {
    ctx.start_activity(
        WorkflowActivities::put_blob,
        PutBlobRequest { bytes },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

fn wait_outcome_name(outcome: AgentWaitOutcome) -> &'static str {
    match outcome {
        AgentWaitOutcome::Terminal => "terminal",
        AgentWaitOutcome::Timeout => "timeout",
        AgentWaitOutcome::Error => "error",
    }
}

fn active_wait_resolution(wait: &ActiveWaitRecord, now_ms: u64) -> Option<AgentWaitOutcome> {
    if wait
        .deadline_ms
        .is_some_and(|deadline_ms| deadline_ms <= now_ms)
    {
        return Some(AgentWaitOutcome::Timeout);
    }
    active_wait_nontimer_resolution(wait)
}

pub(super) fn active_wait_nontimer_resolution(wait: &ActiveWaitRecord) -> Option<AgentWaitOutcome> {
    match wait.mode {
        AgentWaitMode::All => {
            if wait
                .results
                .iter()
                .any(|result| result.status == AgentWaitHandleStatus::Error)
            {
                Some(AgentWaitOutcome::Error)
            } else if wait
                .results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Terminal)
            {
                Some(AgentWaitOutcome::Terminal)
            } else {
                None
            }
        }
        AgentWaitMode::Any => {
            if wait
                .results
                .iter()
                .any(|result| result.status == AgentWaitHandleStatus::Terminal)
            {
                Some(AgentWaitOutcome::Terminal)
            } else if wait
                .results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Error)
            {
                Some(AgentWaitOutcome::Error)
            } else {
                None
            }
        }
    }
}

pub(super) async fn install_deferred_wait(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    deferred: DeferredWait,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let deadline_ms = deferred
        .directive
        .timeout_ms
        .map(|timeout_ms| now.saturating_add(timeout_ms));
    let subscriptions = deferred
        .directive
        .handles
        .iter()
        .filter(|handle| {
            deferred.directive.results.iter().any(|result| {
                result.target_session_id == handle.target_session_id.as_str()
                    && result.run_id == api_run_id(handle.run_id)
                    && result.status == AgentWaitHandleStatus::Pending
            })
        })
        .map(|handle| {
            let subscription = RunSubscription {
                subscription_id: wait_subscription_id(
                    deferred.batch_id,
                    &handle.target_session_id,
                    handle.run_id,
                ),
                subscriber_workflow_id: ctx.workflow_id().to_owned(),
                correlation_token: wait_correlation_token(
                    deferred.batch_id,
                    &handle.target_session_id,
                    handle.run_id,
                ),
                run_id: handle.run_id,
            };
            ActiveWaitSubscription {
                target_session_id: handle.target_session_id.clone(),
                subscription,
            }
        })
        .collect::<Vec<_>>();
    let wait = ActiveWaitRecord {
        batch_id: deferred.batch_id,
        run_id: deferred.run_id,
        turn_id: deferred.turn_id,
        call_id: deferred.directive.call_id,
        mode: deferred.directive.mode,
        handles: deferred.directive.handles,
        results: deferred.directive.results,
        subscriptions: subscriptions.clone(),
        deadline_ms,
    };
    ctx.state_mut(|state| {
        state.active_waits.insert(wait.batch_id.as_u64(), wait);
    });

    for subscription in subscriptions {
        let target_workflow_id = sibling_workflow_id(ctx, &subscription.target_session_id)?;
        let signal_result = ctx
            .external_workflow(target_workflow_id, None)
            .signal(
                AgentSessionWorkflow::subscribe_run,
                subscription.subscription.clone(),
            )
            .await;
        if let Err(error) = signal_result {
            ctx.state_mut(|state| {
                if let Some(wait) = state.active_waits.get_mut(&deferred.batch_id.as_u64()) {
                    mark_wait_handle_error(
                        wait,
                        &subscription.target_session_id,
                        subscription.subscription.run_id,
                        format!("subscribe_run signal failed: {error}"),
                    );
                }
            });
        }
    }
    Ok(())
}

pub(super) async fn process_satisfied_active_waits(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let resolved = ctx.state_mut(|state| {
        let resolved_batch_ids = state
            .active_waits
            .iter()
            .filter_map(|(batch_id, wait)| {
                active_wait_resolution(wait, now).map(|outcome| (*batch_id, outcome))
            })
            .collect::<Vec<_>>();
        resolved_batch_ids
            .into_iter()
            .filter_map(|(batch_id, outcome)| {
                state
                    .active_waits
                    .remove(&batch_id)
                    .map(|wait| (wait, outcome))
            })
            .collect::<Vec<_>>()
    });
    for (wait, outcome) in resolved {
        unsubscribe_wait_subscriptions(ctx, &wait).await;
        let result = build_wait_tool_batch_result(ctx, wait, outcome).await?;
        ctx.state_mut(|state| {
            state
                .pending_tool_batch_resumes
                .push(PendingToolBatchResume {
                    batch_id: result.batch_id,
                    result,
                });
        });
    }
    Ok(())
}

async fn unsubscribe_wait_subscriptions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    wait: &ActiveWaitRecord,
) {
    for subscription in &wait.subscriptions {
        if wait.results.iter().any(|result| {
            result.target_session_id == subscription.target_session_id.as_str()
                && result.run_id == api_run_id(subscription.subscription.run_id)
                && result.status == AgentWaitHandleStatus::Terminal
        }) {
            continue;
        }
        let Ok(target_workflow_id) = sibling_workflow_id(ctx, &subscription.target_session_id)
        else {
            continue;
        };
        let _ = ctx
            .external_workflow(target_workflow_id, None)
            .signal(
                AgentSessionWorkflow::unsubscribe_run,
                subscription.subscription.subscription_id.clone(),
            )
            .await;
    }
}

/// Workflow id of a sibling session in the same universe. Fleet children are
/// spawned through the parent universe's runtime, so cross-session signals
/// compose the parent's own universe prefix (asserted at bootstrap) with the
/// target session id.
fn sibling_workflow_id(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    session_id: &SessionId,
) -> anyhow::Result<String> {
    let Some((universe_id, _)) = crate::split_workflow_id(ctx.workflow_id()) else {
        anyhow::bail!(
            "workflow id is not universe-composed ({{universe_id}}/{{session_id}}): {}",
            ctx.workflow_id()
        );
    };
    Ok(crate::compose_workflow_id(universe_id, session_id))
}

async fn build_wait_tool_batch_result(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    wait: ActiveWaitRecord,
    outcome: AgentWaitOutcome,
) -> anyhow::Result<ToolInvocationBatchResult> {
    let output = AgentWaitOutput {
        outcome,
        results: wait.results,
    };
    let output_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: serde_json::to_vec(&output)?,
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let model_visible_context_entries =
        wait_model_visible_context_entries(ctx, &wait.call_id, &output).await?;
    Ok(ToolInvocationBatchResult {
        run_id: wait.run_id,
        turn_id: wait.turn_id,
        batch_id: wait.batch_id,
        results: vec![ToolInvocationResult {
            call_id: wait.call_id,
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_context_entries,
            error_ref: None,
            effects: Vec::new(),
        }],
    })
}
