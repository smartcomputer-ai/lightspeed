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
            engine::PromiseSource::Timer { .. } => Some((
                promise.promise_id.as_str().to_owned(),
                promise.source.clone(),
            )),
            // Started executions get a slow recovery poll as the backstop
            // for a terminal result whose emission was never observed;
            // bound-receiver promises resolve by pushed emission only.
            engine::PromiseSource::Workflow {
                producer_workflow_kind,
                ..
            } if producer_workflow_kind == engine::WORKFLOW_TOOL_EXECUTION_KIND => Some((
                promise.promise_id.as_str().to_owned(),
                promise.source.clone(),
            )),
            engine::PromiseSource::Workflow { .. } => None,
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

    for mut poll in due {
        let check = match &poll.source {
            engine::PromiseSource::Timer { fire_at_ms } if *fire_at_ms <= now => {
                engine::PromiseSourceCheckResult::Resolved { payload_ref: None }
            }
            engine::PromiseSource::Timer { .. } => {
                advance(&mut poll, now);
                ctx.state_mut(|state| {
                    state
                        .promise_source_polls
                        .insert(poll.promise_id.clone(), poll);
                });
                continue;
            }
            engine::PromiseSource::Workflow {
                producer_workflow_id,
                producer_workflow_kind,
                completion_key,
                ..
            } if producer_workflow_kind == engine::WORKFLOW_TOOL_EXECUTION_KIND => {
                let check = ctx
                    .start_activity(
                        WorkflowActivities::check_workflow_tool_execution,
                        crate::WorkflowToolExecutionCheckRequest {
                            execution_id: producer_workflow_id.clone(),
                            completion_key: completion_key.clone(),
                        },
                        activity_options(),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                if matches!(check, engine::PromiseSourceCheckResult::Pending) {
                    advance(&mut poll, now);
                    ctx.state_mut(|state| {
                        state
                            .promise_source_polls
                            .insert(poll.promise_id.clone(), poll);
                    });
                    continue;
                }
                check
            }
            engine::PromiseSource::Workflow { .. } => continue,
        };
        match check {
            engine::PromiseSourceCheckResult::Pending => {
                if matches!(poll.source, engine::PromiseSource::Timer { .. }) {
                    advance(&mut poll, now);
                    ctx.state_mut(|state| {
                        state
                            .promise_source_polls
                            .insert(poll.promise_id.clone(), poll);
                    });
                }
            }
            engine::PromiseSourceCheckResult::Resolved { payload_ref } => {
                queue_resolution(
                    ctx,
                    poll.promise_id,
                    engine::PromiseResolution::Resolved { payload_ref },
                );
            }
            engine::PromiseSourceCheckResult::Failed { error_ref } => {
                queue_resolution(
                    ctx,
                    poll.promise_id,
                    engine::PromiseResolution::Failed { error_ref },
                );
            }
        }
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

/// Engine-owned hard promise deadlines: a pending promise whose
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
            engine::PromiseResolution::Failed { error_ref: None },
        );
    }
}

pub(super) async fn flush_pending_promise_cancellations(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_promise_cancellations));
    for pending in pending {
        match pending.source {
            engine::PromiseSource::Timer { .. } => {}
            engine::PromiseSource::Workflow {
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
                let Ok(invocation_id) = engine::WorkflowToolInvocationId::try_new(invocation_id)
                else {
                    continue;
                };
                let envelope = EmissionEnvelope::invocation_cancellation(
                    universe_id,
                    session_id,
                    engine::EventSeq::new(pending.log_seq),
                    invocation_id,
                    completion_key,
                    engine::PromiseId::new(pending.promise_id),
                );
                let _ = ctx
                    .external_workflow(producer_workflow_id, None)
                    .signal(AgentSessionWorkflow::deliver_emission, envelope)
                    .await;
            }
        }
    }
    Ok(())
}

/// Producer-authorize and (optionally) schema-validate received
/// source-resolutions, then converge them on the ordinary `ResolvePromise`
/// admission funnel. Resolutions for non-workflow sources pass through
/// unchanged; resolutions for workflow-tool promises must come from the
/// exact stored producer, and `Resolved` payloads must satisfy the
/// binding's immutable reply schema when one is declared.
pub(super) async fn process_pending_source_resolutions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_source_resolutions));
    for pending in pending {
        let PendingSourceResolution {
            promise_id,
            resolution,
            producer,
        } = pending;
        let (authorized, reply_schema_ref) = ctx.state(|state| {
            let Some(promise) = state.core_state.promises.promises.get(&promise_id) else {
                // Unknown promises fall through to ordinary admission,
                // which rejects them with the standard funnel semantics.
                return (true, None);
            };
            let engine::PromiseSource::Workflow {
                producer_workflow_id,
                invocation_id,
                ..
            } = &promise.source
            else {
                return (true, None);
            };
            let producer_matches = matches!(
                &producer,
                engine::EmissionProducer::Workflow { workflow_id, .. }
                    if workflow_id == producer_workflow_id.as_str()
            );
            if !producer_matches {
                return (false, None);
            }
            let reply_schema_ref = engine::WorkflowToolInvocationId::try_new(invocation_id.clone())
                .ok()
                .and_then(|invocation_id| {
                    state
                        .core_state
                        .workflow_tools
                        .emissions
                        .get(&invocation_id)
                        .or_else(|| {
                            state
                                .core_state
                                .workflow_tools
                                .start_requests
                                .get(&invocation_id)
                        })
                })
                .and_then(|invocation| {
                    state
                        .core_state
                        .workflow_tools
                        .bindings
                        .get(&invocation.tool_id)
                })
                .and_then(|binding| match &binding.completion {
                    engine::WorkflowToolCompletion::Joined {
                        reply_schema_ref, ..
                    }
                    | engine::WorkflowToolCompletion::Promises {
                        reply_schema_ref, ..
                    } => reply_schema_ref.clone(),
                    engine::WorkflowToolCompletion::Accepted => None,
                });
            (true, reply_schema_ref)
        });
        if !authorized {
            ctx.state_mut(|state| {
                state.last_error = Some(format!(
                    "unauthorized producer for workflow-tool promise {promise_id}"
                ));
            });
            continue;
        }
        let resolution = match (reply_schema_ref, resolution) {
            (Some(reply_schema_ref), engine::PromiseResolution::Resolved { payload_ref }) => {
                let validation = ctx
                    .start_activity(
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
                        engine::PromiseResolution::Resolved { payload_ref }
                    }
                    crate::WorkflowToolReplyValidationResult::Invalid { error_ref } => {
                        engine::PromiseResolution::Failed {
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
    resolution: engine::PromiseResolution,
) {
    ctx.state_mut(|state| {
        let promise_id = match engine::PromiseId::try_new(promise_id) {
            Ok(promise_id) => promise_id,
            Err(error) => {
                state.last_error = Some(format!("cannot queue promise resolution: {error}"));
                return;
            }
        };
        state.queue_admission(AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id,
                resolution,
            },
            correlation_token: None,
        });
    });
}

/// The started-execution recovery poll is a slow backstop behind the
/// primary pushed-emission path, not a delivery mechanism.
const WORKFLOW_EXECUTION_RECOVERY_POLL_MS: u64 = 10_000;

fn initial_check_at_ms(source: &engine::PromiseSource, now_ms: u64) -> u64 {
    match source {
        engine::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        engine::PromiseSource::Workflow { .. } => now_ms + WORKFLOW_EXECUTION_RECOVERY_POLL_MS,
    }
}

fn advance(poll: &mut PromiseSourcePoll, now_ms: u64) {
    poll.poll_attempt = poll.poll_attempt.saturating_add(1);
    poll.next_check_at_ms = match &poll.source {
        engine::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        engine::PromiseSource::Workflow { .. } => now_ms + WORKFLOW_EXECUTION_RECOVERY_POLL_MS,
    };
}
