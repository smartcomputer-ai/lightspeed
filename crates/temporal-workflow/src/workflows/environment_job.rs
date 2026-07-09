use std::time::Duration;

use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD, EnvironmentJobCancelActivityRequest,
    EnvironmentJobCancelSignal, EnvironmentJobPollActivityRequest, EnvironmentJobWorkflowArgs,
    EnvironmentJobWorkflowSnapshot, WorkflowActivities, activity_options,
};

#[workflow(name = "EnvironmentJobWorkflow")]
pub struct EnvironmentJobWorkflow {
    snapshot: EnvironmentJobWorkflowSnapshot,
    pending_cancels: Vec<EnvironmentJobCancelSignal>,
    nudged: bool,
}

impl Default for EnvironmentJobWorkflow {
    fn default() -> Self {
        Self {
            snapshot: EnvironmentJobWorkflowSnapshot::default(),
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
        args: EnvironmentJobWorkflowArgs,
    ) -> WorkflowResult<()> {
        ctx.state_mut(|state| {
            state.snapshot.instance_id = args.instance_id.clone();
            state.snapshot.job_group_id = args.job_group_id.clone();
        });

        loop {
            let cancels = ctx.state_mut(|state| std::mem::take(&mut state.pending_cancels));
            for cancel in cancels {
                if let Err(error) = ctx
                    .start_activity(
                        WorkflowActivities::environment_job_cancel,
                        EnvironmentJobCancelActivityRequest {
                            instance_id: args.instance_id.clone(),
                            jobs: cancel.jobs,
                            scope: cancel.scope,
                            force: cancel.force,
                        },
                        activity_options(),
                    )
                    .await
                {
                    ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                }
            }

            match ctx
                .start_activity(
                    WorkflowActivities::environment_job_poll,
                    EnvironmentJobPollActivityRequest {
                        instance_id: args.instance_id.clone(),
                        job_group_id: args.job_group_id.clone(),
                        job_ids: args.job_ids.clone(),
                    },
                    activity_options(),
                )
                .await
            {
                Ok(result) => {
                    let terminal = result.terminal;
                    ctx.state_mut(|state| {
                        state.snapshot.jobs = result.jobs;
                        state.snapshot.terminal = terminal;
                        state.snapshot.last_error = None;
                    });
                    if terminal {
                        return Ok(());
                    }
                }
                Err(error) => {
                    ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                }
            }

            if ctx.continue_as_new_suggested()
                || ctx.history_length() >= DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD
            {
                let mut next = args.clone();
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

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> EnvironmentJobWorkflowSnapshot {
        self.snapshot.clone()
    }
}
