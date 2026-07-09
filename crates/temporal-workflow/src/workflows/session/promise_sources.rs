use super::*;

const INITIAL_POLL_DELAY_MS: u64 = 2_000;
const FAST_POLL_ATTEMPTS: u32 = 30;
const MEDIUM_POLL_ATTEMPTS: u32 = 70;
const MEDIUM_POLL_DELAY_MS: u64 = 15_000;
const MAX_POLL_DELAY_MS: u64 = 60_000;

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
            engine::PromiseSource::EnvJob { .. } | engine::PromiseSource::Timer { .. } => Some((
                promise.promise_id.as_str().to_owned(),
                promise.source.clone(),
            )),
            engine::PromiseSource::Run { .. } => None,
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

pub(super) fn record_environment_job_changed(
    workflow: &mut AgentSessionWorkflow,
    changed: EnvironmentJobChanged,
) {
    for poll in workflow.promise_source_polls.values_mut() {
        let engine::PromiseSource::EnvJob {
            instance_id,
            job_id,
            ..
        } = &poll.source
        else {
            continue;
        };
        if instance_id == &changed.instance_id && job_id == &changed.job_id {
            poll.next_check_at_ms = 0;
        }
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
                advance(&mut poll, now);
                ctx.state_mut(|state| {
                    state
                        .promise_source_polls
                        .insert(poll.promise_id.clone(), poll);
                });
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
        engine::PromiseSource::EnvJob { .. } => now_ms.saturating_add(poll_delay_ms(0)),
        engine::PromiseSource::Run { .. } => now_ms,
    }
}

fn advance(poll: &mut PromiseSourcePoll, now_ms: u64) {
    poll.poll_attempt = poll.poll_attempt.saturating_add(1);
    poll.next_check_at_ms = match &poll.source {
        engine::PromiseSource::Timer { fire_at_ms } => *fire_at_ms,
        engine::PromiseSource::EnvJob { .. } => {
            now_ms.saturating_add(poll_delay_ms(poll.poll_attempt))
        }
        engine::PromiseSource::Run { .. } => now_ms,
    };
}

fn poll_delay_ms(poll_attempt: u32) -> u64 {
    if poll_attempt < FAST_POLL_ATTEMPTS {
        INITIAL_POLL_DELAY_MS
    } else if poll_attempt < MEDIUM_POLL_ATTEMPTS {
        MEDIUM_POLL_DELAY_MS
    } else {
        MAX_POLL_DELAY_MS
    }
}
