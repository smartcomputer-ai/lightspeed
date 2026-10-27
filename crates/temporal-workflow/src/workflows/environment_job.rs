use crate::workflows::WorkflowContextExt as _;
use std::time::Duration;

use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, LocalActivityOptions, SyncWorkflowContext, WorkflowContext,
    WorkflowContextView, WorkflowResult,
};

use crate::{
    AgentSessionWorkflow, DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
    EnvironmentJobCancelActivityRequest, EnvironmentJobCancelScope, EnvironmentJobCancelSignal,
    EnvironmentJobId, EnvironmentJobObservation, EnvironmentJobPollActivityRequest,
    EnvironmentJobPollActivityResult, EnvironmentJobRecord, EnvironmentJobStatus,
    EnvironmentJobSubscription, EnvironmentJobWorkflowInput, EnvironmentJobWorkflowSnapshot,
    EnvironmentJobWorkflowToolContext, WorkflowActivities, WorkflowToolRecoveryResult,
    compose_environment_job_workflow_id,
};

#[workflow]
#[derive(Default)]
pub struct EnvironmentJobWorkflow {
    snapshot: EnvironmentJobWorkflowSnapshot,
    subscriptions: Vec<EnvironmentJobSubscription>,
    pending_cancels: Vec<EnvironmentJobCancelSignal>,
    workflow_tool: Option<EnvironmentJobWorkflowToolContext>,
    nudged: bool,
}

#[workflow_methods]
impl EnvironmentJobWorkflow {
    #[run(name = "EnvironmentJobWorkflow")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: EnvironmentJobWorkflowInput,
    ) -> WorkflowResult<()> {
        let mut args = match input {
            EnvironmentJobWorkflowInput::Job(args) => args,
            EnvironmentJobWorkflowInput::WorkflowTool(start) => {
                if ctx.workflow_id() != start.execution_id
                    || start.universe_id != start.invocation.session_universe_id
                {
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "environment job workflow-tool execution identity is invalid"
                    ))
                    .into());
                }
                ctx.execute_local_activity(
                    WorkflowActivities::environment_job_prepare_workflow_tool,
                    crate::EnvironmentJobPrepareWorkflowToolRequest { start },
                    environment_job_activity_options(),
                )
                .await
                .map_err(|error| {
                    temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "prepare environment job workflow: {error}"
                    ))
                })?
            }
        };
        let expected_workflow_id = args
            .workflow_tool
            .as_ref()
            .map(|tool| tool.execution_id.clone())
            .unwrap_or_else(|| {
                compose_environment_job_workflow_id(
                    args.universe_id,
                    &args.start.environment_id,
                    &args.start.job_group_id,
                )
            });
        if args.start.universe_id != args.universe_id || ctx.workflow_id() != expected_workflow_id {
            return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                "environment job workflow id does not match its universe and job identity: workflow_id={} expected={}",
                ctx.workflow_id(),
                expected_workflow_id
            ))
            .into());
        }
        ctx.state_mut(|state| {
            state.snapshot.environment_id = args.start.environment_id.clone();
            state.snapshot.job_group_id = args.start.job_group_id.clone();
            state.snapshot.started = args.started;
            state.snapshot.jobs = args.jobs.clone();
            state.snapshot.resolutions = args.resolutions.clone();
            refresh_terminal(&mut state.snapshot, &args.job_ids);
            state.workflow_tool = args.workflow_tool.clone();
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
                .execute_local_activity(
                    WorkflowActivities::environment_job_start,
                    args.start.clone(),
                    environment_job_activity_options(),
                )
                .await
            {
                Ok(result) => {
                    args.job_ids = result.jobs.iter().map(|job| job.job_id.clone()).collect();
                    ctx.state_mut(|state| {
                        state.snapshot.started = true;
                        state.snapshot.jobs = result.jobs;
                        state.snapshot.last_error = None;
                    });
                }
                Err(error) => {
                    ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                    return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                        "environment job start failed: {error}"
                    ))
                    .into());
                }
            }
        }

        loop {
            let cancels = ctx.state_mut(|state| std::mem::take(&mut state.pending_cancels));
            for cancel in cancels {
                match ctx
                    .execute_local_activity(
                        WorkflowActivities::environment_job_cancel,
                        EnvironmentJobCancelActivityRequest {
                            universe_id: args.universe_id,
                            environment_id: args.start.environment_id.clone(),
                            jobs: cancel.jobs,
                            scope: cancel.scope,
                            force: cancel.force,
                        },
                        environment_job_activity_options(),
                    )
                    .await
                {
                    Ok(result) => ctx.state_mut(|state| {
                        apply_observations(&mut state.snapshot, &args.job_ids, result.jobs);
                        refresh_terminal(&mut state.snapshot, &args.job_ids);
                        state.snapshot.last_error = None;
                    }),
                    Err(error) => {
                        ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                    }
                }
            }

            let unsettled = ctx.state(|state| unsettled_job_ids(&state.snapshot, &args.job_ids));
            if !unsettled.is_empty() {
                match ctx
                    .execute_local_activity(
                        WorkflowActivities::environment_job_poll,
                        EnvironmentJobPollActivityRequest {
                            universe_id: args.universe_id,
                            environment_id: args.start.environment_id.clone(),
                            job_group_id: args.start.job_group_id.clone(),
                            job_ids: unsettled,
                        },
                        environment_job_activity_options(),
                    )
                    .await
                {
                    Ok(result) => {
                        ctx.state_mut(|state| {
                            apply_poll_result(&mut state.snapshot, &args.job_ids, result);
                            state.snapshot.last_error = None;
                        });
                    }
                    Err(error) => {
                        ctx.state_mut(|state| state.snapshot.last_error = Some(error.to_string()));
                    }
                }
            }

            flush_terminal_emissions(ctx, args.universe_id).await;
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
                ctx.continue_as_new(
                    EnvironmentJobWorkflowInput::Job(next),
                    ContinueAsNewOptions::default(),
                )?;
            }

            ctx.state_mut(|state| state.nudged = false);
            let was_cancelled = {
                let wait =
                    ctx.wait_for_state(|state| state.nudged || !state.pending_cancels.is_empty());
                let timer = ctx
                    .timer_with_manual_cancellation(Duration::from_millis(args.poll_ms.max(250)))
                    .fuse();
                let cancelled = ctx.cancelled().fuse();
                pin_mut!(wait, timer, cancelled);
                select! {
                    _ = wait => false,
                    _ = timer => false,
                    _ = cancelled => true,
                }
            };
            if was_cancelled {
                cancel_workflow_jobs(ctx, &args).await;
                return Err(temporalio_sdk::WorkflowTermination::cancelled());
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

    /// Per-key cancellation for internally supervised job starts.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        envelope: harness::EmissionEnvelope,
    ) {
        queue_workflow_tool_cancellation(self, envelope);
    }

    #[signal(name = "nudge")]
    pub fn nudge(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        self.nudged = true;
    }

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> EnvironmentJobWorkflowSnapshot {
        self.snapshot.clone()
    }

    #[query(name = "workflow_tool_recovery")]
    pub fn workflow_tool_recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        workflow_tool_recovery(self)
    }
}

async fn cancel_workflow_jobs(
    ctx: &mut WorkflowContext<EnvironmentJobWorkflow>,
    args: &crate::EnvironmentJobWorkflowArgs,
) {
    let jobs = ctx.state(|state| {
        state
            .snapshot
            .jobs
            .iter()
            .filter(|job| !job.status.is_terminal())
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>()
    });
    let jobs = if jobs.is_empty() && !ctx.state(|state| state.snapshot.started) {
        args.job_ids.clone()
    } else {
        jobs
    };
    if jobs.is_empty() {
        return;
    }
    let _ = ctx
        .execute_local_activity(
            WorkflowActivities::environment_job_cancel,
            EnvironmentJobCancelActivityRequest {
                universe_id: args.universe_id,
                environment_id: args.start.environment_id.clone(),
                jobs,
                scope: EnvironmentJobCancelScope::Job,
                force: false,
            },
            environment_job_activity_options(),
        )
        .await;
}

/// Apply a poll: record the resolutions, then the observations. A job the
/// daemon answered `notFound` for is `lost`; when the activity returned no
/// resolution for it, the job still settles as a failure without a payload,
/// so its subscribers are released.
fn apply_poll_result(
    snapshot: &mut EnvironmentJobWorkflowSnapshot,
    job_ids: &[EnvironmentJobId],
    result: EnvironmentJobPollActivityResult,
) {
    snapshot.resolutions.extend(
        result
            .resolutions
            .into_iter()
            .filter(|(job_id, _)| job_ids.iter().any(|id| id.as_str() == job_id)),
    );
    for observation in &result.jobs {
        if let EnvironmentJobObservation::NotFound { job_id } = observation
            && job_ids.contains(job_id)
        {
            snapshot
                .resolutions
                .entry(job_id.as_str().to_owned())
                .or_insert(harness::PromiseSourceCheckResult::Failed { error_ref: None });
        }
    }
    apply_observations(snapshot, job_ids, result.jobs);
    refresh_terminal(snapshot, job_ids);
}

/// A summary replaces the job's record; `notFound` marks the job `lost`,
/// keeping what the workflow already knew about it. Observations of jobs
/// outside the group (such as dependents a cancel reached) are ignored.
fn apply_observations(
    snapshot: &mut EnvironmentJobWorkflowSnapshot,
    job_ids: &[EnvironmentJobId],
    observations: Vec<EnvironmentJobObservation>,
) {
    for observation in observations {
        if !job_ids.contains(observation.job_id()) {
            continue;
        }
        let existing = snapshot
            .jobs
            .iter_mut()
            .find(|existing| &existing.job_id == observation.job_id());
        match (observation, existing) {
            (EnvironmentJobObservation::Job { record }, Some(existing)) => *existing = record,
            (EnvironmentJobObservation::Job { record }, None) => snapshot.jobs.push(record),
            (EnvironmentJobObservation::NotFound { .. }, Some(existing)) => {
                existing.status = EnvironmentJobStatus::Lost;
            }
            (EnvironmentJobObservation::NotFound { job_id }, None) => {
                snapshot.jobs.push(EnvironmentJobRecord::lost(job_id));
            }
        }
    }
}

/// Jobs of the group without both a terminal record and a resolution.
fn unsettled_job_ids(
    snapshot: &EnvironmentJobWorkflowSnapshot,
    job_ids: &[EnvironmentJobId],
) -> Vec<EnvironmentJobId> {
    job_ids
        .iter()
        .filter(|job_id| {
            let terminal = snapshot
                .jobs
                .iter()
                .any(|job| &job.job_id == *job_id && job.status.is_terminal());
            !(terminal && snapshot.resolutions.contains_key(job_id.as_str()))
        })
        .cloned()
        .collect()
}

fn refresh_terminal(snapshot: &mut EnvironmentJobWorkflowSnapshot, job_ids: &[EnvironmentJobId]) {
    snapshot.terminal = !job_ids.is_empty() && unsettled_job_ids(snapshot, job_ids).is_empty();
}

/// Environment jobs deliberately remain co-located with the core environment
/// registry and environment adapter. Their short, idempotent adapter calls therefore
/// run as local activities: Temporal still records completion and retries, but
/// does not route the calls through a separately versioned activity worker.
fn environment_job_activity_options() -> LocalActivityOptions {
    LocalActivityOptions::builder()
        .schedule_to_close_timeout(crate::config::DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT)
        .start_to_close_timeout(crate::config::DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT)
        .cancellation_token(temporalio_sdk::WorkflowCancellationToken::new())
        .build()
}

fn queue_workflow_tool_cancellation(
    state: &mut EnvironmentJobWorkflow,
    envelope: harness::EmissionEnvelope,
) {
    let Some(workflow_tool) = &state.workflow_tool else {
        return;
    };
    let producer_workflow_id = match &envelope.producer {
        harness::EmissionProducer::Session {
            universe_id,
            session_id,
            ..
        } => crate::compose_workflow_id(*universe_id, session_id),
        harness::EmissionProducer::Workflow { .. } => return,
    };
    let harness::EmissionBody::InvocationCancellation {
        invocation_id,
        completion_key,
        promise_id,
    } = envelope.body
    else {
        return;
    };
    if invocation_id != workflow_tool.invocation_id {
        return;
    }
    let Some(subscription) = state.subscriptions.iter().find(|subscription| {
        subscription.completion_key == completion_key
            && subscription.promise_id == promise_id.as_str()
            && subscription.holder_workflow_id == producer_workflow_id
    }) else {
        return;
    };
    if !state
        .pending_cancels
        .iter()
        .any(|pending| pending.jobs.len() == 1 && pending.jobs[0] == subscription.job_id)
    {
        state.pending_cancels.push(EnvironmentJobCancelSignal {
            jobs: vec![subscription.job_id.clone()],
            scope: EnvironmentJobCancelScope::Job,
            force: false,
        });
    }
    state.nudged = true;
}

async fn flush_terminal_emissions(
    ctx: &mut WorkflowContext<EnvironmentJobWorkflow>,
    universe_id: uuid::Uuid,
) {
    let workflow_id = ctx.workflow_id().to_owned();
    let emissions =
        ctx.state_mut(|state| collect_terminal_emissions(state, universe_id, &workflow_id));
    for (receiver_workflow_id, envelope) in emissions {
        let _ = ctx
            .external_workflow(receiver_workflow_id, None)
            .signal(
                AgentSessionWorkflow::deliver_emission,
                envelope,
                crate::workflows::signal_options(),
            )
            .await;
    }
}

fn collect_terminal_emissions(
    state: &mut EnvironmentJobWorkflow,
    universe_id: uuid::Uuid,
    workflow_id: &str,
) -> Vec<(String, harness::EmissionEnvelope)> {
    let mut emissions = Vec::new();
    for subscription in &mut state.subscriptions {
        if subscription.notified {
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
        let resolution = match result {
            harness::PromiseSourceCheckResult::Pending => continue,
            harness::PromiseSourceCheckResult::Resolved { payload_ref } => {
                harness::PromiseResolution::Resolved { payload_ref }
            }
            harness::PromiseSourceCheckResult::Failed { error_ref } => {
                harness::PromiseResolution::Failed { error_ref }
            }
        };
        let Ok(promise_id) = harness::PromiseId::try_new(subscription.promise_id.clone()) else {
            continue;
        };
        subscription.notified = true;
        let holder_workflow_id = subscription.holder_workflow_id.clone();
        emissions.push((
            holder_workflow_id.clone(),
            harness::EmissionEnvelope::source_resolution(
                universe_id,
                workflow_id.to_owned(),
                &holder_workflow_id,
                promise_id,
                resolution,
            ),
        ));
    }
    emissions
}

fn workflow_tool_recovery(state: &EnvironmentJobWorkflow) -> WorkflowToolRecoveryResult {
    let mut resolutions = std::collections::BTreeMap::new();
    for subscription in &state.subscriptions {
        let Some(result) = state.snapshot.resolutions.get(subscription.job_id.as_str()) else {
            continue;
        };
        let resolution = match result {
            harness::PromiseSourceCheckResult::Pending => continue,
            harness::PromiseSourceCheckResult::Resolved { payload_ref } => {
                harness::PromiseResolution::Resolved {
                    payload_ref: payload_ref.clone(),
                }
            }
            harness::PromiseSourceCheckResult::Failed { error_ref } => {
                harness::PromiseResolution::Failed {
                    error_ref: error_ref.clone(),
                }
            }
        };
        resolutions.insert(subscription.completion_key.clone(), resolution);
    }
    WorkflowToolRecoveryResult { resolutions }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use harness::{BlobRef, PromiseSourceCheckResult};

    use super::*;

    fn subscription() -> EnvironmentJobSubscription {
        EnvironmentJobSubscription {
            holder_workflow_id: "universe/session_1".to_owned(),
            promise_id: "promise_1".to_owned(),
            completion_key: "job_1".to_owned(),
            job_id: EnvironmentJobId::new("job_1"),
            notified: false,
        }
    }

    #[test]
    fn terminal_subscription_notifies_once() {
        let mut workflow = EnvironmentJobWorkflow::default();
        workflow.subscriptions.push(subscription());
        let payload_ref = BlobRef::from_bytes(b"done");
        workflow.snapshot.resolutions.insert(
            "job_1".to_owned(),
            PromiseSourceCheckResult::Resolved {
                payload_ref: Some(payload_ref.clone()),
            },
        );
        assert!(matches!(
            workflow_tool_recovery(&workflow).resolutions.get("job_1"),
            Some(harness::PromiseResolution::Resolved {
                payload_ref: Some(actual),
            }) if actual == &payload_ref
        ));

        let universe_id = uuid::Uuid::from_u128(1);
        let first = collect_terminal_emissions(&mut workflow, universe_id, "universe/envjob-job_1");
        let second =
            collect_terminal_emissions(&mut workflow, universe_id, "universe/envjob-job_1");

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "universe/session_1");
        assert!(matches!(
            &first[0].1.body,
            harness::EmissionBody::SourceResolution {
                promise_id,
                resolution: harness::PromiseResolution::Resolved {
                    payload_ref: Some(actual),
                },
            } if promise_id.as_str() == "promise_1" && actual == &payload_ref
        ));
        assert!(matches!(
            first[0].1.producer,
            harness::EmissionProducer::Workflow {
                universe_id: actual,
                ref workflow_id,
            } if actual == universe_id && workflow_id == "universe/envjob-job_1"
        ));
        assert!(second.is_empty());
    }

    fn record(job_id: &str, status: EnvironmentJobStatus) -> EnvironmentJobRecord {
        EnvironmentJobRecord {
            job_id: EnvironmentJobId::new(job_id),
            name: Some(job_id.to_owned()),
            status,
            depends_on: Vec::new(),
            queue_key: None,
        }
    }

    #[test]
    fn not_found_settles_the_job_as_lost() {
        let job_ids = vec![
            EnvironmentJobId::new("job_1"),
            EnvironmentJobId::new("job_2"),
        ];
        let mut snapshot = EnvironmentJobWorkflowSnapshot {
            started: true,
            jobs: vec![
                record("job_1", EnvironmentJobStatus::Running),
                record("job_2", EnvironmentJobStatus::Queued),
            ],
            ..Default::default()
        };
        assert_eq!(unsettled_job_ids(&snapshot, &job_ids), job_ids);

        let done = BlobRef::from_bytes(b"done");
        apply_poll_result(
            &mut snapshot,
            &job_ids,
            EnvironmentJobPollActivityResult {
                jobs: vec![
                    EnvironmentJobObservation::Job {
                        record: record("job_1", EnvironmentJobStatus::Succeeded),
                    },
                    EnvironmentJobObservation::NotFound {
                        job_id: EnvironmentJobId::new("job_2"),
                    },
                    EnvironmentJobObservation::NotFound {
                        job_id: EnvironmentJobId::new("elsewhere"),
                    },
                ],
                resolutions: BTreeMap::from([(
                    "job_1".to_owned(),
                    PromiseSourceCheckResult::Resolved {
                        payload_ref: Some(done.clone()),
                    },
                )]),
            },
        );

        assert!(snapshot.terminal);
        assert!(unsettled_job_ids(&snapshot, &job_ids).is_empty());
        assert_eq!(snapshot.jobs.len(), 2);
        assert_eq!(snapshot.jobs[1].status, EnvironmentJobStatus::Lost);
        // The record keeps what the workflow knew before the job was lost.
        assert_eq!(snapshot.jobs[1].name.as_deref(), Some("job_2"));
        assert_eq!(
            snapshot.resolutions.get("job_2"),
            Some(&PromiseSourceCheckResult::Failed { error_ref: None })
        );
        assert!(!snapshot.resolutions.contains_key("elsewhere"));
    }

    #[test]
    fn activity_resolution_of_a_lost_job_wins_and_running_jobs_stay_unsettled() {
        let job_ids = vec![
            EnvironmentJobId::new("job_1"),
            EnvironmentJobId::new("job_2"),
        ];
        let mut snapshot = EnvironmentJobWorkflowSnapshot::default();
        let lost = BlobRef::from_bytes(b"lost");
        apply_poll_result(
            &mut snapshot,
            &job_ids,
            EnvironmentJobPollActivityResult {
                jobs: vec![
                    EnvironmentJobObservation::NotFound {
                        job_id: EnvironmentJobId::new("job_1"),
                    },
                    EnvironmentJobObservation::Job {
                        record: record("job_2", EnvironmentJobStatus::Running),
                    },
                ],
                resolutions: BTreeMap::from([(
                    "job_1".to_owned(),
                    PromiseSourceCheckResult::Failed {
                        error_ref: Some(lost.clone()),
                    },
                )]),
            },
        );

        assert!(!snapshot.terminal);
        assert_eq!(
            snapshot.jobs[0],
            EnvironmentJobRecord::lost(EnvironmentJobId::new("job_1"))
        );
        assert_eq!(
            snapshot.resolutions.get("job_1"),
            Some(&PromiseSourceCheckResult::Failed {
                error_ref: Some(lost)
            })
        );
        assert_eq!(
            unsettled_job_ids(&snapshot, &job_ids),
            vec![EnvironmentJobId::new("job_2")]
        );
    }

    #[test]
    fn cancel_not_found_marks_lost_but_waits_for_the_poll_resolution() {
        let job_ids = vec![EnvironmentJobId::new("job_1")];
        let mut snapshot = EnvironmentJobWorkflowSnapshot {
            jobs: vec![record("job_1", EnvironmentJobStatus::Running)],
            ..Default::default()
        };
        apply_observations(
            &mut snapshot,
            &job_ids,
            vec![
                EnvironmentJobObservation::NotFound {
                    job_id: EnvironmentJobId::new("job_1"),
                },
                EnvironmentJobObservation::Job {
                    record: record("dependent", EnvironmentJobStatus::Cancelled),
                },
            ],
        );
        refresh_terminal(&mut snapshot, &job_ids);

        assert_eq!(snapshot.jobs.len(), 1);
        assert_eq!(snapshot.jobs[0].status, EnvironmentJobStatus::Lost);
        assert!(!snapshot.terminal);
        assert_eq!(unsettled_job_ids(&snapshot, &job_ids), job_ids);
    }

    #[test]
    fn workflow_tool_cancellation_targets_only_the_matching_job() {
        let invocation_id =
            harness::WorkflowToolInvocationId::new(format!("wti:sha256:{}", "a".repeat(64)));
        let universe_id = uuid::Uuid::from_u128(1);
        let session_id = harness::SessionId::new("session_1");
        let mut workflow = EnvironmentJobWorkflow {
            workflow_tool: Some(EnvironmentJobWorkflowToolContext {
                execution_id: "execution_1".to_owned(),
                invocation_id: invocation_id.clone(),
            }),
            ..Default::default()
        };
        let mut subscription = subscription();
        subscription.holder_workflow_id = crate::compose_workflow_id(universe_id, &session_id);
        workflow.subscriptions.push(subscription);

        queue_workflow_tool_cancellation(
            &mut workflow,
            harness::EmissionEnvelope::invocation_cancellation(
                universe_id,
                session_id.clone(),
                harness::EventSeq::new(7),
                invocation_id.clone(),
                "job_1".to_owned(),
                harness::PromiseId::new("promise_1"),
            ),
        );

        assert_eq!(workflow.pending_cancels.len(), 1);
        assert_eq!(
            workflow.pending_cancels[0].jobs,
            vec![EnvironmentJobId::new("job_1")]
        );
        assert!(workflow.nudged);

        queue_workflow_tool_cancellation(
            &mut workflow,
            harness::EmissionEnvelope::invocation_cancellation(
                universe_id,
                session_id,
                harness::EventSeq::new(8),
                invocation_id,
                "job_1".to_owned(),
                harness::PromiseId::new("promise_1"),
            ),
        );
        assert_eq!(workflow.pending_cancels.len(), 1);
    }
}
