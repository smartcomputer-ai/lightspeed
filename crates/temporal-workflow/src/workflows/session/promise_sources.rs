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
            engine::PromiseSource::EnvJob { .. } | engine::PromiseSource::Run { .. } => None,
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

pub(super) fn has_unconfirmed_subscriptions(state: &AgentSessionWorkflow) -> bool {
    state.core_state.promises.pending().any(|promise| {
        matches!(promise.source, engine::PromiseSource::EnvJob { .. })
            && !state
                .confirmed_promise_source_subscriptions
                .contains(promise.promise_id.as_str())
    })
}

pub(super) async fn process_unconfirmed_subscriptions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| {
        let pending_ids = state
            .core_state
            .promises
            .pending()
            .filter(|promise| matches!(promise.source, engine::PromiseSource::EnvJob { .. }))
            .map(|promise| promise.promise_id.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        state
            .confirmed_promise_source_subscriptions
            .retain(|promise_id| pending_ids.contains(promise_id));
        state
            .core_state
            .promises
            .pending()
            .filter(|promise| matches!(promise.source, engine::PromiseSource::EnvJob { .. }))
            .filter(|promise| {
                !state
                    .confirmed_promise_source_subscriptions
                    .contains(promise.promise_id.as_str())
            })
            .map(|promise| {
                (
                    promise.promise_id.as_str().to_owned(),
                    promise.source.clone(),
                )
            })
            .collect::<Vec<_>>()
    });

    for (promise_id, source) in pending {
        let result = ctx
            .start_activity(
                WorkflowActivities::subscribe_promise_source,
                engine::PromiseSourceSubscribeRequest {
                    source,
                    holder_workflow_id: ctx.workflow_id().to_owned(),
                    promise_id: promise_id.clone(),
                },
                activity_options(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        ctx.state_mut(|state| {
            state
                .confirmed_promise_source_subscriptions
                .insert(promise_id.clone());
        });
        match result {
            engine::PromiseSourceCheckResult::Pending => {}
            engine::PromiseSourceCheckResult::Resolved { payload_ref } => queue_resolution(
                ctx,
                promise_id,
                engine::PromiseResolution::Resolved { payload_ref },
            ),
            engine::PromiseSourceCheckResult::Failed { error_ref } => queue_resolution(
                ctx,
                promise_id,
                engine::PromiseResolution::Failed { error_ref },
            ),
        }
    }
    Ok(())
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
            engine::PromiseSource::EnvJob { .. } => ctx
                .start_activity(
                    WorkflowActivities::check_promise_source,
                    engine::PromiseSourceCheckRequest {
                        source: poll.source.clone(),
                    },
                    activity_options(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?,
            engine::PromiseSource::Run { .. } => continue,
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
    Ok(())
}

pub(super) async fn flush_pending_promise_cancellations(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_promise_cancellations));
    for pending in pending {
        match pending.source {
            engine::PromiseSource::Run {
                target_session_id,
                target_run_id,
            } => {
                let Some((universe_id, _)) = split_workflow_id(ctx.workflow_id()) else {
                    continue;
                };
                let Ok(session_id) = engine::SessionId::try_new(target_session_id) else {
                    continue;
                };
                let workflow_id = compose_workflow_id(universe_id, &session_id);
                let admission = AgentAdmission {
                    command: CoreAgentCommand::CancelRun {
                        run_id: engine::RunId::new(target_run_id),
                    },
                    context_key: None,
                };
                let _ = ctx
                    .external_workflow(workflow_id, None)
                    .signal(AgentSessionWorkflow::submit_admissions, vec![admission])
                    .await;
            }
            engine::PromiseSource::EnvJob { .. } => {
                let _ = ctx
                    .start_activity(
                        WorkflowActivities::cancel_promise_source,
                        engine::PromiseSourceCancelRequest {
                            source: pending.source,
                        },
                        activity_options(),
                    )
                    .await;
            }
            engine::PromiseSource::Timer { .. } => {}
        }
    }
    Ok(())
}

fn queue_resolution(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    promise_id: String,
    resolution: engine::PromiseResolution,
) {
    ctx.state_mut(|state| {
        state.pending_admissions.push(AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id: engine::PromiseId::new(promise_id),
                resolution,
            },
            context_key: None,
        });
    });
}

fn initial_check_at_ms(source: &engine::PromiseSource, now_ms: u64) -> u64 {
    match source {
        engine::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        engine::PromiseSource::EnvJob { .. } => now_ms,
        engine::PromiseSource::Run { .. } => now_ms,
    }
}

fn advance(poll: &mut PromiseSourcePoll, now_ms: u64) {
    poll.poll_attempt = poll.poll_attempt.saturating_add(1);
    poll.next_check_at_ms = match &poll.source {
        engine::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        engine::PromiseSource::EnvJob { .. } => now_ms,
        engine::PromiseSource::Run { .. } => now_ms,
    };
}
