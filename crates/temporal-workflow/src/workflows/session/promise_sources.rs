use super::*;

pub(super) fn reconcile_polls(ctx: &WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    ctx.state_mut(|state| {
        reconcile_polls_for_state(state, now);
    });
}

pub(super) fn reconcile_polls_for_state(state: &mut AgentSessionWorkflow, now_ms: u64) {
    let pending = state
        .core_state
        .promises
        .pending()
        .filter_map(|promise| match &promise.source {
            harness::PromiseSource::Timer { .. } => Some((
                promise.promise_id.as_str().to_owned(),
                promise.source.clone(),
            )),
            // Started executions get a slow recovery poll as the backstop
            // for a terminal result whose emission was never observed;
            // bound-receiver promises resolve by pushed emission only.
            harness::PromiseSource::Workflow {
                producer_workflow_kind,
                ..
            } if producer_workflow_kind == harness::WORKFLOW_TOOL_EXECUTION_KIND => Some((
                promise.promise_id.as_str().to_owned(),
                promise.source.clone(),
            )),
            harness::PromiseSource::Workflow { .. } => None,
        })
        .collect::<BTreeMap<_, _>>();
    state
        .promise_source_polls
        .retain(|promise_id, _| pending.contains_key(promise_id));
    for (promise_id, source) in pending {
        state
            .promise_source_polls
            .entry(promise_id.clone())
            .or_insert_with(|| PromiseSourcePoll {
                promise_id,
                next_check_at_ms: initial_check_at_ms(&source, now_ms),
                poll_attempt: 0,
                source,
            });
    }
}

pub(super) fn has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    state
        .promise_source_polls
        .values()
        .any(|poll| poll.next_check_at_ms == 0)
}

pub(super) fn nearest_wake_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .promise_source_polls
        .values()
        .map(|poll| poll.next_check_at_ms)
        .min()
}

pub(super) async fn process_due(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let due = ctx.state_mut(|state| {
        let due_ids = state
            .promise_source_polls
            .iter()
            .filter_map(|(promise_id, poll)| {
                (poll.next_check_at_ms <= now).then_some(promise_id.clone())
            })
            .collect::<Vec<_>>();
        due_ids
            .into_iter()
            .filter_map(|promise_id| state.promise_source_polls.remove(&promise_id))
            .collect::<Vec<_>>()
    });
    let had_due = !due.is_empty();

    // All keys of an invocation describe one execution, so one answer per sweep says
    // what a running execution's other keys would hear.
    let mut pending_executions = BTreeSet::new();
    for poll in due {
        // Only started executions and timers get a poll entry.
        let (producer_workflow_id, completion_key) = match &poll.source {
            harness::PromiseSource::Timer { fire_at_ms } => {
                if *fire_at_ms <= now {
                    queue_resolution(
                        ctx,
                        poll.promise_id,
                        harness::PromiseResolution::Resolved { payload_ref: None },
                    );
                } else {
                    reinstall(ctx, poll, now);
                }
                continue;
            }
            harness::PromiseSource::Workflow {
                producer_workflow_id,
                completion_key,
                ..
            } => (producer_workflow_id.clone(), completion_key.clone()),
        };
        if pending_executions.contains(&producer_workflow_id) {
            reinstall(ctx, poll, now);
            continue;
        }
        let check = ctx
            .execute_activity(
                WorkflowActivities::check_workflow_tool_execution,
                crate::WorkflowToolExecutionCheckRequest {
                    execution_id: producer_workflow_id.clone(),
                    completion_key,
                },
                crate::recovery_poll_activity_options(),
            )
            .await;
        let resolution = match check {
            Ok(harness::PromiseSourceCheckResult::Pending) => {
                pending_executions.insert(producer_workflow_id);
                reinstall(ctx, poll, now);
                continue;
            }
            // A backstop that cannot run now loses nothing by asking again later.
            Err(error) => {
                record_warning(
                    ctx,
                    format!("recovery check of execution {producer_workflow_id} failed: {error}"),
                );
                pending_executions.insert(producer_workflow_id);
                reinstall(ctx, poll, now);
                continue;
            }
            Ok(harness::PromiseSourceCheckResult::Resolved { payload_ref }) => {
                harness::PromiseResolution::Resolved { payload_ref }
            }
            Ok(harness::PromiseSourceCheckResult::Failed { error_ref }) => {
                harness::PromiseResolution::Failed { error_ref }
            }
        };
        let Some((universe_id, _)) = split_workflow_id(ctx.workflow_id()) else {
            continue;
        };
        // The same queue as a signal, so the reply schema applies and a signal that
        // arrived during this check stays ahead of what the check found.
        ctx.state_mut(|state| {
            state
                .pending_source_resolutions
                .push(PendingSourceResolution {
                    promise_id: harness::PromiseId::new(poll.promise_id),
                    resolution,
                    producer: harness::EmissionProducer::Workflow {
                        universe_id,
                        workflow_id: producer_workflow_id,
                    },
                });
        });
    }
    if had_due {
        // A completed poll with its next schedule installed is a safe
        // rollover checkpoint even when it produced no session-log append.
        // Without this marker, a long pending Promise source could grow
        // Temporal history forever after a continuation.
        ctx.state_mut(|state| state.execution_has_rollover_checkpoint = true);
    }
    Ok(())
}

/// Harness-owned hard promise deadlines: a pending promise whose
/// `deadline_ms` has passed fails through the ordinary `ResolvePromise`
/// funnel (first-writer-wins keeps a racing real resolution harmless). An
/// `await` timeout never changes the underlying promise; this is the
/// promise's own deadline.
pub(super) fn nearest_promise_deadline_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .core_state
        .promises
        .pending()
        .filter_map(|promise| promise.deadline_ms)
        .min()
}

pub(super) fn has_due_promise_deadline(state: &AgentSessionWorkflow, now_ms: u64) -> bool {
    state.core_state.promises.pending().any(|promise| {
        promise
            .deadline_ms
            .is_some_and(|deadline| deadline <= now_ms)
    })
}

pub(super) fn process_due_promise_deadlines(ctx: &mut WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    let due = ctx.state(|state| {
        state
            .core_state
            .promises
            .pending()
            .filter(|promise| promise.deadline_ms.is_some_and(|deadline| deadline <= now))
            .map(|promise| promise.promise_id.as_str().to_owned())
            .collect::<Vec<_>>()
    });
    for promise_id in due {
        queue_resolution(
            ctx,
            promise_id,
            harness::PromiseResolution::Failed {
                error_ref: Some(harness::promise_deadline_exceeded_ref()),
            },
        );
    }
}

pub(super) async fn flush_pending_promise_cancellations(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_promise_cancellations));
    for pending in pending {
        match pending.source {
            harness::PromiseSource::Timer { .. } => {}
            harness::PromiseSource::Workflow {
                producer_workflow_id,
                invocation_id,
                completion_key,
                ..
            } => {
                // Best-effort per-key cancellation fact to the bound
                // receiver: the promise is already terminal in the session,
                // so a late reply is a no-op. This never cancels the shared
                // receiver workflow execution.
                let Some((universe_id, _)) = split_workflow_id(ctx.workflow_id()) else {
                    continue;
                };
                let Some(session_id) = ctx.state(|state| state.session_id.clone()) else {
                    continue;
                };
                let Ok(invocation_id) = harness::WorkflowToolInvocationId::try_new(invocation_id)
                else {
                    continue;
                };
                let envelope = EmissionEnvelope::invocation_cancellation(
                    universe_id,
                    session_id,
                    harness::EventSeq::new(pending.log_seq),
                    invocation_id,
                    completion_key,
                    harness::PromiseId::new(pending.promise_id),
                );
                let _ = ctx
                    .external_workflow(producer_workflow_id, None)
                    .signal(
                        AgentSessionWorkflow::deliver_emission,
                        envelope,
                        crate::workflows::signal_options(),
                    )
                    .await;
            }
        }
    }
    Ok(())
}

/// What the session does with one received source-resolution.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SourceResolutionGate {
    /// Admit it, once a `Resolved` payload satisfies this schema if one is declared.
    Admit { reply_schema_ref: Option<BlobRef> },
    /// Nothing to do, and any reason is already recorded.
    Drop,
}

/// Only the exact stored producer of a workflow-tool promise may settle it, and
/// nothing outside the session settles a timer. A delivery for a terminal promise is
/// the designed late reply. One for an absent promise can be late too, since state
/// prunes old promises, so neither may disturb the session.
pub(super) fn source_resolution_gate(
    state: &mut AgentSessionWorkflow,
    replaying: bool,
    pending: &PendingSourceResolution,
) -> SourceResolutionGate {
    let promise_id = &pending.promise_id;
    let Some(promise) = state.core_state.promises.promises.get(promise_id) else {
        state.record_warning(
            replaying,
            format!("source resolution for unknown promise {promise_id} ignored"),
        );
        return SourceResolutionGate::Drop;
    };
    if promise.status.is_terminal() {
        return SourceResolutionGate::Drop;
    }
    let authorized_invocation = match &promise.source {
        harness::PromiseSource::Workflow {
            producer_workflow_id,
            invocation_id,
            ..
        } if matches!(
            &pending.producer,
            harness::EmissionProducer::Workflow { workflow_id, .. }
                if workflow_id == producer_workflow_id.as_str()
        ) =>
        {
            Some(invocation_id.clone())
        }
        _ => None,
    };
    let Some(invocation_id) = authorized_invocation else {
        state.record_warning(
            replaying,
            format!("unauthorized producer for promise {promise_id}"),
        );
        return SourceResolutionGate::Drop;
    };
    let tools = &state.core_state.workflow_tools;
    let reply_schema_ref = harness::WorkflowToolInvocationId::try_new(invocation_id)
        .ok()
        .and_then(|invocation_id| {
            tools
                .emissions
                .get(&invocation_id)
                .or_else(|| tools.start_requests.get(&invocation_id))
        })
        .and_then(|invocation| tools.bindings.get(&invocation.tool_id))
        .and_then(|binding| match &binding.completion {
            harness::WorkflowToolCompletion::Joined {
                reply_schema_ref, ..
            }
            | harness::WorkflowToolCompletion::Promises {
                reply_schema_ref, ..
            } => reply_schema_ref.clone(),
            harness::WorkflowToolCompletion::Accepted => None,
        });
    SourceResolutionGate::Admit { reply_schema_ref }
}

/// Producer-authorize and (optionally) schema-validate received
/// source-resolutions, then converge them on the ordinary `ResolvePromise`
/// admission funnel. `Resolved` payloads must satisfy the binding's immutable
/// reply schema when one is declared.
pub(super) async fn process_pending_source_resolutions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| {
        let pending = std::mem::take(&mut state.pending_source_resolutions);
        // A refused producer appends nothing and its signal still grew the history.
        state.execution_has_rollover_checkpoint |= !pending.is_empty();
        pending
    });
    let replaying = ctx.is_replaying();
    for pending in pending {
        let gate = ctx.state_mut(|state| source_resolution_gate(state, replaying, &pending));
        let SourceResolutionGate::Admit { reply_schema_ref } = gate else {
            continue;
        };
        let PendingSourceResolution {
            promise_id,
            resolution,
            ..
        } = pending;
        let resolution = match (reply_schema_ref, resolution) {
            (Some(reply_schema_ref), harness::PromiseResolution::Resolved { payload_ref }) => {
                let validation = ctx
                    .execute_activity(
                        WorkflowActivities::validate_workflow_tool_reply,
                        crate::WorkflowToolReplyValidationRequest {
                            reply_schema_ref,
                            payload_ref: payload_ref.clone(),
                        },
                        activity_options(),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                match validation {
                    crate::WorkflowToolReplyValidationResult::Valid => {
                        harness::PromiseResolution::Resolved { payload_ref }
                    }
                    crate::WorkflowToolReplyValidationResult::Invalid { error_ref } => {
                        harness::PromiseResolution::Failed {
                            error_ref: Some(error_ref),
                        }
                    }
                }
            }
            (_, resolution) => resolution,
        };
        queue_resolution(ctx, promise_id.as_str().to_owned(), resolution);
    }
    Ok(())
}

fn queue_resolution(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    promise_id: String,
    resolution: harness::PromiseResolution,
) {
    let promise_id = match harness::PromiseId::try_new(promise_id) {
        Ok(promise_id) => promise_id,
        Err(error) => {
            record_warning(ctx, format!("cannot queue promise resolution: {error}"));
            return;
        }
    };
    ctx.state_mut(|state| {
        state.queue_admission(AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id,
                resolution,
            },
            correlation_token: None,
        });
    });
}

/// The started-execution recovery poll is a backstop behind the primary
/// pushed-emission path, not a delivery mechanism. Each poll is an activity in
/// history, so a long-running execution is asked less and less often.
const WORKFLOW_EXECUTION_RECOVERY_POLL_MS: u64 = 10_000;
const WORKFLOW_EXECUTION_RECOVERY_POLL_MAX_MS: u64 = 60_000;

fn initial_check_at_ms(source: &harness::PromiseSource, now_ms: u64) -> u64 {
    match source {
        harness::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        harness::PromiseSource::Workflow { .. } => now_ms + WORKFLOW_EXECUTION_RECOVERY_POLL_MS,
    }
}

pub(super) fn advance(poll: &mut PromiseSourcePoll, now_ms: u64) {
    poll.poll_attempt = poll.poll_attempt.saturating_add(1);
    poll.next_check_at_ms = match &poll.source {
        harness::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        harness::PromiseSource::Workflow { .. } => {
            let doublings = 1u64.checked_shl(poll.poll_attempt).unwrap_or(u64::MAX);
            let backoff = WORKFLOW_EXECUTION_RECOVERY_POLL_MS
                .saturating_mul(doublings)
                .min(WORKFLOW_EXECUTION_RECOVERY_POLL_MAX_MS);
            now_ms.saturating_add(backoff)
        }
    };
}

fn reinstall(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    mut poll: PromiseSourcePoll,
    now_ms: u64,
) {
    advance(&mut poll, now_ms);
    ctx.state_mut(|state| {
        state
            .promise_source_polls
            .insert(poll.promise_id.clone(), poll);
    });
}
