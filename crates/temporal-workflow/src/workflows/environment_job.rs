use std::{
    collections::BTreeSet,
    time::{Duration, UNIX_EPOCH},
};

use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    AgentSessionWorkflow, DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
    EnvironmentJobCancelActivityRequest, EnvironmentJobCancelSignal,
    EnvironmentJobConfirmSubscriptionSignal, EnvironmentJobPollActivityRequest,
    EnvironmentJobSubscription, EnvironmentJobWorkflowArgs, EnvironmentJobWorkflowSnapshot,
    PromiseSourceResolutionSignal, WorkflowActivities, activity_options,
};

const SUBSCRIPTION_CONFIRMATION_TIMEOUT_MS: u64 = 60_000;

#[workflow(name = "EnvironmentJobWorkflow")]
pub struct EnvironmentJobWorkflow {
    snapshot: EnvironmentJobWorkflowSnapshot,
    subscriptions: Vec<EnvironmentJobSubscription>,
    pending_cancels: Vec<EnvironmentJobCancelSignal>,
    nudged: bool,
}

impl Default for EnvironmentJobWorkflow {
    fn default() -> Self {
        Self {
            snapshot: EnvironmentJobWorkflowSnapshot::default(),
            subscriptions: Vec::new(),
            pending_cancels: Vec::new(),
            nudged: false,
        }
    }
}

#[workflow_methods]
impl EnvironmentJobWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        mut args: EnvironmentJobWorkflowArgs,
    ) -> WorkflowResult<()> {
        ctx.state_mut(|state| {
            state.snapshot.instance_id = args.start.instance_id.clone();
            state.snapshot.job_group_id = args.start.job_group_id.clone();
            state.snapshot.started = args.started;
            state.snapshot.jobs = args.jobs.clone();
            state.snapshot.resolutions = args.resolutions.clone();
            for subscription in args.subscriptions.drain(..) {
                if !state.subscriptions.iter().any(|existing| {
                    existing.holder_workflow_id == subscription.holder_workflow_id
                        && existing.promise_id == subscription.promise_id
                }) {
                    state.subscriptions.push(subscription);
                }
            }
        });

        if !ctx.state(|state| state.snapshot.started) {
            match ctx
                .start_activity(
                    WorkflowActivities::environment_job_start,
                    args.start.clone(),
                    activity_options(),
                )
                .await
            {
                Ok(result) => {
                    args.job_ids = result.jobs.iter().map(|job| job.job_id.clone()).collect();
                    let confirmation_deadline_ms =
                        workflow_time_ms(ctx).saturating_add(SUBSCRIPTION_CONFIRMATION_TIMEOUT_MS);
                    ctx.state_mut(|state| {
                        state.snapshot.started = true;
                        state.snapshot.jobs = result.jobs;
                        state.snapshot.last_error = None;
                        for subscription in &mut state.subscriptions {
                            if subscription.confirmation_deadline_ms == 0 {
                                subscription.confirmation_deadline_ms = confirmation_deadline_ms;
                            }
                        }
                    });
                }
                Err(error) => {
                    ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                    return Err(anyhow::anyhow!("environment job start failed: {error}").into());
                }
            }
        }

        loop {
            expire_unconfirmed_subscriptions(ctx);
            let cancels = ctx.state_mut(|state| std::mem::take(&mut state.pending_cancels));
            for cancel in cancels {
                match ctx
                    .start_activity(
                        WorkflowActivities::environment_job_cancel,
                        EnvironmentJobCancelActivityRequest {
                            instance_id: args.start.instance_id.clone(),
                            jobs: cancel.jobs,
                            scope: cancel.scope,
                            force: cancel.force,
                        },
                        activity_options(),
                    )
                    .await
                {
                    Ok(jobs) => ctx.state_mut(|state| {
                        for job in jobs {
                            if let Some(existing) = state
                                .snapshot
                                .jobs
                                .iter_mut()
                                .find(|existing| existing.job_id == job.job_id)
                            {
                                *existing = job;
                            }
                        }
                        state.snapshot.last_error = None;
                    }),
                    Err(error) => {
                        ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                    }
                }
            }

            if !ctx.state(|state| state.snapshot.terminal) {
                match ctx
                    .start_activity(
                        WorkflowActivities::environment_job_poll,
                        EnvironmentJobPollActivityRequest {
                            instance_id: args.start.instance_id.clone(),
                            job_group_id: args.start.job_group_id.clone(),
                            job_ids: args.job_ids.clone(),
                        },
                        activity_options(),
                    )
                    .await
                {
                    Ok(result) => {
                        ctx.state_mut(|state| {
                            state.snapshot.jobs = result.jobs;
                            state.snapshot.resolutions.extend(result.resolutions);
                            state.snapshot.terminal = result.terminal;
                            state.snapshot.last_error = None;
                        });
                    }
                    Err(error) => {
                        ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                    }
                }
            }

            flush_terminal_notifications(ctx).await;
            if ctx.state(|state| {
                state.snapshot.terminal
                    && state
                        .subscriptions
                        .iter()
                        .all(|subscription| subscription.notified)
            }) {
                return Ok(());
            }

            if ctx.continue_as_new_suggested()
                || ctx.history_length() >= DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD
            {
                let mut next = args.clone();
                ctx.state(|state| {
                    next.started = state.snapshot.started;
                    next.jobs = state.snapshot.jobs.clone();
                    next.resolutions = state.snapshot.resolutions.clone();
                    next.subscriptions = state.subscriptions.clone();
                });
                next.poll_attempt = next.poll_attempt.saturating_add(1);
                ctx.continue_as_new(&next, ContinueAsNewOptions::default())?;
            }

            ctx.state_mut(|state| state.nudged = false);
            let wait =
                ctx.wait_condition(|state| state.nudged || !state.pending_cancels.is_empty());
            let timer = ctx
                .timer(Duration::from_millis(args.poll_ms.max(250)))
                .fuse();
            pin_mut!(wait, timer);
            select! {
                _ = wait => {},
                _ = timer => {},
            }
        }
    }

    #[signal(name = "cancel_jobs")]
    pub fn cancel_jobs(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        signal: EnvironmentJobCancelSignal,
    ) {
        self.pending_cancels.push(signal);
        self.nudged = true;
    }

    #[signal(name = "nudge")]
    pub fn nudge(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        self.nudged = true;
    }

    #[signal(name = "confirm_subscription")]
    pub fn confirm_subscription(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        signal: EnvironmentJobConfirmSubscriptionSignal,
    ) {
        if let Some(subscription) = self.subscriptions.iter_mut().find(|subscription| {
            subscription.holder_workflow_id == signal.holder_workflow_id
                && subscription.promise_id == signal.promise_id
                && subscription.job_id == signal.job_id
        }) {
            subscription.confirmed = true;
        }
        self.nudged = true;
    }

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> EnvironmentJobWorkflowSnapshot {
        self.snapshot.clone()
    }
}

fn expire_unconfirmed_subscriptions(ctx: &WorkflowContext<EnvironmentJobWorkflow>) {
    let now_ms = workflow_time_ms(ctx);
    ctx.state_mut(|state| expire_unconfirmed_subscriptions_for_state(state, now_ms));
}

async fn flush_terminal_notifications(ctx: &mut WorkflowContext<EnvironmentJobWorkflow>) {
    let notifications = ctx.state_mut(collect_terminal_notifications);
    for (holder_workflow_id, signal) in notifications {
        let _ = ctx
            .external_workflow(holder_workflow_id, None)
            .signal(AgentSessionWorkflow::resolve_promise_source, signal)
            .await;
    }
}

fn expire_unconfirmed_subscriptions_for_state(state: &mut EnvironmentJobWorkflow, now_ms: u64) {
    let expired_jobs = state
        .subscriptions
        .iter()
        .filter(|subscription| {
            !subscription.confirmed && subscription.confirmation_deadline_ms <= now_ms
        })
        .filter(|expired| {
            !state
                .subscriptions
                .iter()
                .any(|subscription| subscription.confirmed && subscription.job_id == expired.job_id)
        })
        .map(|subscription| subscription.job_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if expired_jobs.is_empty() {
        return;
    }
    state.subscriptions.retain(|subscription| {
        subscription.confirmed || subscription.confirmation_deadline_ms > now_ms
    });
    state.pending_cancels.push(EnvironmentJobCancelSignal {
        jobs: expired_jobs,
        scope: host_protocol::data::jobs::JobCancelScope::Job,
        force: false,
    });
    state.nudged = true;
}

fn collect_terminal_notifications(
    state: &mut EnvironmentJobWorkflow,
) -> Vec<(String, PromiseSourceResolutionSignal)> {
    let mut notifications = Vec::new();
    for subscription in &mut state.subscriptions {
        if !subscription.confirmed || subscription.notified {
            continue;
        }
        let Some(result) = state
            .snapshot
            .resolutions
            .get(subscription.job_id.as_str())
            .cloned()
        else {
            continue;
        };
        subscription.notified = true;
        notifications.push((
            subscription.holder_workflow_id.clone(),
            PromiseSourceResolutionSignal {
                promise_id: subscription.promise_id.clone(),
                result,
            },
        ));
    }
    notifications
}

fn workflow_time_ms(ctx: &WorkflowContext<EnvironmentJobWorkflow>) -> u64 {
    ctx.workflow_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use engine::{BlobRef, PromiseSourceCheckResult};
    use host_protocol::shared::JobId;

    use super::*;

    fn subscription(confirmed: bool) -> EnvironmentJobSubscription {
        EnvironmentJobSubscription {
            holder_workflow_id: "universe/session_1".to_owned(),
            promise_id: "promise_1".to_owned(),
            job_id: JobId::new("job_1"),
            confirmation_deadline_ms: 100,
            confirmed,
            notified: false,
        }
    }

    #[test]
    fn expired_unconfirmed_subscription_cancels_job() {
        let mut workflow = EnvironmentJobWorkflow::default();
        workflow.subscriptions.push(subscription(false));

        expire_unconfirmed_subscriptions_for_state(&mut workflow, 100);

        assert!(workflow.subscriptions.is_empty());
        assert_eq!(workflow.pending_cancels.len(), 1);
        assert_eq!(workflow.pending_cancels[0].jobs, vec![JobId::new("job_1")]);
    }

    #[test]
    fn confirmed_terminal_subscription_notifies_once() {
        let mut workflow = EnvironmentJobWorkflow::default();
        workflow.subscriptions.push(subscription(true));
        let payload_ref = BlobRef::from_bytes(b"done");
        workflow.snapshot.resolutions.insert(
            "job_1".to_owned(),
            PromiseSourceCheckResult::Resolved {
                payload_ref: Some(payload_ref.clone()),
            },
        );

        let first = collect_terminal_notifications(&mut workflow);
        let second = collect_terminal_notifications(&mut workflow);

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "universe/session_1");
        assert_eq!(first[0].1.promise_id, "promise_1");
        assert_eq!(
            first[0].1.result,
            PromiseSourceCheckResult::Resolved {
                payload_ref: Some(payload_ref),
            }
        );
        assert!(second.is_empty());
    }
}
