use super::*;

/// The parked tool batch, if any, derived from typed core state.
pub(super) struct ParkedToolBatch {
    pub run_id: engine::RunId,
    pub batch_id: engine::ToolBatchId,
    pub suspension: engine::ToolBatchSuspension,
}

impl ParkedToolBatch {
    pub(super) fn spec(&self) -> &engine::AwaitSpec {
        self.suspension.spec()
    }
}

pub(super) fn parked_tool_batch(core_state: &CoreAgentState) -> Option<ParkedToolBatch> {
    let active_run = core_state.runs.active.as_ref()?;
    let parked = active_run.parked_tool_batch.as_ref()?;
    let batch = active_run.tool_batches.get(&parked.batch_id)?;
    Some(ParkedToolBatch {
        run_id: batch.run_id,
        batch_id: parked.batch_id,
        suspension: parked.suspension.clone(),
    })
}

pub(super) fn has_satisfied_await(state: &AgentSessionWorkflow) -> bool {
    if state
        .pending_tool_batch_resumes
        .iter()
        .any(|resume| Some(resume.batch_id) == parked_tool_batch_batch_id(&state.core_state))
    {
        return false;
    }
    let Some(parked) = parked_tool_batch(&state.core_state) else {
        return false;
    };
    let non_timeout_ms = parked
        .spec()
        .deadline_at_ms
        .map_or(u64::MAX, |deadline| deadline.saturating_sub(1));
    engine::await_wake(&state.core_state, non_timeout_ms).is_some()
}

fn parked_tool_batch_batch_id(core_state: &CoreAgentState) -> Option<engine::ToolBatchId> {
    parked_tool_batch(core_state).map(|parked| parked.batch_id)
}

pub(super) fn nearest_await_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    parked_tool_batch(&state.core_state).and_then(|parked| parked.spec().deadline_at_ms)
}

/// Resume a parked tool batch when its Promise wait is satisfied. Explicit
/// await snapshots model-owned Promises into blobs; Joined resumes directly
/// from runtime-owned Promise state.
pub(super) async fn process_satisfied_await(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let resolved = ctx.state(|state| {
        let parked = parked_tool_batch(&state.core_state)?;
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
            engine::WakeReason::Timeout => AwaitOutcome::Timeout,
            engine::WakeReason::Terminal => AwaitOutcome::Terminal,
        };
        let results = promise_snapshot(parked.spec(), &state.core_state);
        Some((parked, claim, outcome, results))
    });
    let Some((parked, claim, outcome, results)) = resolved else {
        return Ok(());
    };

    // The runtime prepares what the resolved payloads supply beside the
    // results (a sub-agent's linked images, for example); the engine only
    // records and sequences those entries.
    let has_resolved_payload = results
        .iter()
        .any(|result| result.status == "resolved" && result.payload_ref.is_some());
    let resume_output = match &parked.suspension {
        engine::ToolBatchSuspension::AwaitTool { .. } => {
            let request = AwaitMaterializationRequest { outcome, results };
            let materialized = ctx
                .start_activity(
                    WorkflowActivities::materialize_await_result,
                    request,
                    activity_options(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            engine::ToolBatchResumeOutput::AwaitTool {
                result_ref: materialized.result_ref,
                additional_context: materialized.additional_context,
            }
        }
        engine::ToolBatchSuspension::JoinedWorkflowCalls { .. } => {
            let additional_context = if has_resolved_payload {
                ctx.start_activity(
                    WorkflowActivities::prepare_joined_context,
                    JoinedContextPreparationRequest { results },
                    activity_options(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?
            } else {
                Vec::new()
            };
            engine::ToolBatchResumeOutput::JoinedWorkflowCalls { additional_context }
        }
    };
    let command = engine::ResumeToolBatchCommand {
        run_id: parked.run_id,
        batch_id: parked.batch_id,
        claim,
        claim_observed_at_ms: now,
        output: resume_output,
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
