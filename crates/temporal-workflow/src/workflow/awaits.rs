use super::*;

/// The parked `await` call, if any, derived from typed core state. At most one
/// can exist: a session has one active run with one active parked await.
pub(super) struct ParkedAwait {
    pub run_id: engine::RunId,
    pub batch_id: engine::ToolBatchId,
    pub spec: engine::AwaitSpec,
}

pub(super) fn parked_await(core_state: &CoreAgentState) -> Option<ParkedAwait> {
    let active_run = core_state.runs.active.as_ref()?;
    let parked = active_run.parked_await.as_ref()?;
    let batch = active_run.tool_batches.get(&parked.batch_id)?;
    Some(ParkedAwait {
        run_id: batch.run_id,
        batch_id: parked.batch_id,
        spec: parked.spec.clone(),
    })
}

pub(super) fn has_satisfied_await(state: &AgentSessionWorkflow) -> bool {
    if state
        .pending_tool_batch_resumes
        .iter()
        .any(|resume| Some(resume.batch_id) == parked_await_batch_id(&state.core_state))
    {
        return false;
    }
    let Some(parked) = parked_await(&state.core_state) else {
        return false;
    };
    let non_timeout_ms = parked
        .spec
        .deadline_at_ms
        .map_or(u64::MAX, |deadline| deadline.saturating_sub(1));
    engine::await_wake(&state.core_state, non_timeout_ms).is_some()
}

fn parked_await_batch_id(core_state: &CoreAgentState) -> Option<engine::ToolBatchId> {
    parked_await(core_state).map(|parked| parked.batch_id)
}

pub(super) fn nearest_await_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    parked_await(&state.core_state).and_then(|parked| parked.spec.deadline_at_ms)
}

/// Resolve the parked await if its mode or deadline is satisfied: snapshot
/// every requested promise (total outcome), blob the output, and queue the
/// deferred-batch resume. Timeout leaves the remaining promises pending and
/// re-awaitable.
pub(super) async fn process_satisfied_await(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let resolved = ctx.state(|state| {
        let parked = parked_await(&state.core_state)?;
        if state
            .pending_tool_batch_resumes
            .iter()
            .any(|resume| resume.batch_id == parked.batch_id)
        {
            return None;
        }
        let claim = engine::await_wake(&state.core_state, now)?;
        let outcome = match claim {
            engine::WakeReason::Cancelled => AwaitOutcome::Cancelled,
            engine::WakeReason::MailboxMessage => AwaitOutcome::MailboxMessage,
            engine::WakeReason::Timeout => AwaitOutcome::Timeout,
            engine::WakeReason::Terminal => AwaitOutcome::Terminal,
        };
        let mailbox_messages = if claim == engine::WakeReason::MailboxMessage {
            buffered_mailbox_messages(&state.core_state)
        } else {
            Vec::new()
        };
        let results = promise_snapshot(&parked.spec, &state.core_state);
        Some((parked, claim, outcome, results, mailbox_messages))
    });
    let Some((parked, claim, outcome, results, mailbox_messages)) = resolved else {
        return Ok(());
    };

    let output = AwaitOutput {
        outcome,
        results,
        mailbox_messages,
    };
    let output_ref = put_await_blob(ctx, serde_json::to_vec(&output)?).await?;
    let summary_ref = put_await_blob(ctx, await_summary(&output).into_bytes()).await?;
    let command = engine::ResumeAwaitCommand {
        run_id: parked.run_id,
        batch_id: parked.batch_id,
        claim,
        claim_observed_at_ms: now,
        output: engine::AwaitOutputRefs {
            output_ref,
            summary_ref,
        },
    };
    ctx.state_mut(|state| {
        state
            .pending_tool_batch_resumes
            .push(PendingToolBatchResume {
                batch_id: parked.batch_id,
                command,
            });
    });
    Ok(())
}

fn buffered_mailbox_messages(core_state: &CoreAgentState) -> Vec<ContextEntryInput> {
    core_state
        .runs
        .messages
        .iter()
        .filter(|message| message.status == engine::MessageStatus::Buffered)
        .flat_map(|message| message.input.iter().cloned())
        .collect()
}

pub(super) fn promise_snapshot(
    spec: &engine::AwaitSpec,
    core_state: &CoreAgentState,
) -> Vec<AwaitPromiseResult> {
    spec.promise_ids
        .iter()
        .map(
            |promise_id| match core_state.promises.promises.get(promise_id) {
                Some(promise) => AwaitPromiseResult {
                    promise_id: promise_id.as_str().to_owned(),
                    status: promise_status_name(promise.status).to_owned(),
                    payload_ref: promise.payload_ref.clone(),
                    error_ref: promise.error_ref.clone(),
                },
                None => AwaitPromiseResult {
                    promise_id: promise_id.as_str().to_owned(),
                    status: "unknown".to_owned(),
                    payload_ref: None,
                    error_ref: None,
                },
            },
        )
        .collect()
}

pub(super) fn promise_status_name(status: engine::PromiseStatus) -> &'static str {
    match status {
        engine::PromiseStatus::Pending => "pending",
        engine::PromiseStatus::Resolved => "resolved",
        engine::PromiseStatus::Failed => "failed",
        engine::PromiseStatus::Cancelled => "cancelled",
    }
}

fn await_summary(output: &AwaitOutput) -> String {
    let terminal = output
        .results
        .iter()
        .filter(|result| result.status != "pending")
        .count();
    let pending = output.results.len() - terminal;
    let outcome = match output.outcome {
        AwaitOutcome::Terminal => "terminal",
        AwaitOutcome::Timeout => "timeout",
        AwaitOutcome::Cancelled => "cancelled",
        AwaitOutcome::MailboxMessage => "mailbox_message",
    };
    let detail = output
        .results
        .iter()
        .map(|result| format!("{}: {}", result.promise_id, result.status))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "await resolved with outcome {outcome} (terminal: {terminal}, pending: {pending}). {detail}"
    )
}

async fn put_await_blob(
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
