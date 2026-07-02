use super::*;

impl AgentSessionWorkflow {
    pub fn queue_admission(&mut self, admission: AgentAdmission) {
        self.pending_admissions.push(admission);
    }

    pub fn subscribe_to_run(&mut self, subscription: RunSubscription) {
        if let Some(notification) =
            terminal_notification_for_state(&self.core_state, subscription.run_id)
        {
            self.pending_terminal_notifications
                .push(PendingRunTerminalNotification {
                    notification: notification
                        .with_correlation_token(subscription.correlation_token.clone()),
                    subscription,
                });
            return;
        }
        self.run_subscriptions
            .insert(subscription.subscription_id.clone(), subscription);
    }

    pub fn unsubscribe_from_run(&mut self, subscription_id: &str) {
        self.run_subscriptions.remove(subscription_id);
        self.pending_terminal_notifications
            .retain(|pending| pending.subscription.subscription_id != subscription_id);
    }

    pub fn record_run_terminal(&mut self, notification: RunTerminalNotification) {
        for wait in self.active_waits.values_mut() {
            mark_wait_terminal_arrival(wait, &notification);
        }
    }

    pub fn record_environment_job_changed(&mut self, changed: EnvironmentJobChanged) {
        job_waits::record_changed(self, changed);
    }

    pub(super) fn queue_terminal_notifications_for_entries(&mut self, entries: &[CoreAgentEntry]) {
        for entry in entries {
            if let Some(notification) = terminal_notification_for_event(&entry.event.kind) {
                self.queue_terminal_notifications(notification);
            }
        }
    }

    pub(super) fn queue_terminal_notifications(&mut self, notification: RunTerminalNotification) {
        let subscription_ids = self
            .run_subscriptions
            .iter()
            .filter_map(|(subscription_id, subscription)| {
                (subscription.run_id == notification.run_id).then_some(subscription_id.clone())
            })
            .collect::<Vec<_>>();
        for subscription_id in subscription_ids {
            let Some(subscription) = self.run_subscriptions.remove(&subscription_id) else {
                continue;
            };
            self.pending_terminal_notifications
                .push(PendingRunTerminalNotification {
                    notification: notification
                        .clone()
                        .with_correlation_token(subscription.correlation_token.clone()),
                    subscription,
                });
        }
    }

    pub fn status_snapshot(&self) -> AgentSessionStatus {
        AgentSessionStatus {
            session_id: self
                .session_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            initialized: self.initialized,
            pending_admissions: self.pending_admissions.len(),
            pending_tool_batch_resumes: self.pending_tool_batch_resumes.len(),
            active_waits: self.active_waits.len() + self.active_environment_job_waits.len(),
            run_subscriptions: self.run_subscriptions.len(),
            active_run: self
                .core_state
                .runs
                .active
                .as_ref()
                .map(|run| AgentActiveRunSummary {
                    run_id: run.run_id.as_u64(),
                    status: run.status,
                    submission_id: run.submission_id.clone(),
                    output_ref: run.output_ref.clone(),
                    active_turn_id: run.active_turn_id.map(|id| id.as_u64()),
                    active_tool_batch_id: run.active_tool_batch_id.map(|id| id.as_u64()),
                }),
            queued_runs: self
                .core_state
                .runs
                .queued
                .iter()
                .map(|run| AgentQueuedRunSummary {
                    run_id: run.run_id.as_u64(),
                    submission_id: run.submission_id.clone(),
                    input: run.source.input().to_vec(),
                })
                .collect(),
            completed_runs: self
                .core_state
                .runs
                .completed
                .iter()
                .map(|run| AgentCompletedRunSummary {
                    run_id: run.run_id.as_u64(),
                    status: run.status,
                    submission_id: self
                        .run_submissions
                        .get(&run.run_id.as_u64())
                        .cloned()
                        .flatten(),
                    output_ref: run.output_ref.clone(),
                    failure_message_ref: run
                        .failure
                        .as_ref()
                        .and_then(|failure| failure.message_ref.clone()),
                })
                .collect(),
            admission_failures: self.admission_failures.clone(),
            last_error: self.last_error.clone(),
            bootstrap_failed: self.bootstrap_failed,
        }
    }
}

impl RunTerminalNotification {
    fn with_correlation_token(mut self, correlation_token: String) -> Self {
        self.correlation_token = correlation_token;
        self
    }
}

fn terminal_notification_for_state(
    state: &CoreAgentState,
    run_id: engine::RunId,
) -> Option<RunTerminalNotification> {
    state
        .runs
        .completed
        .iter()
        .find(|record| record.run_id == run_id)
        .map(|record| RunTerminalNotification {
            correlation_token: String::new(),
            run_id,
            status: record.status,
            output_ref: record.output_ref.clone(),
            failure_message_ref: record
                .failure
                .as_ref()
                .and_then(|failure| failure.message_ref.clone()),
        })
}

fn terminal_notification_for_event(event: &CoreAgentEventKind) -> Option<RunTerminalNotification> {
    match event {
        CoreAgentEventKind::Run(RunEvent::Completed { run_id, output_ref }) => {
            Some(RunTerminalNotification {
                correlation_token: String::new(),
                run_id: *run_id,
                status: RunStatus::Completed,
                output_ref: output_ref.clone(),
                failure_message_ref: None,
            })
        }
        CoreAgentEventKind::Run(RunEvent::Failed { run_id, failure }) => {
            Some(RunTerminalNotification {
                correlation_token: String::new(),
                run_id: *run_id,
                status: RunStatus::Failed,
                output_ref: None,
                failure_message_ref: failure.message_ref.clone(),
            })
        }
        CoreAgentEventKind::Run(RunEvent::Cancelled { run_id }) => Some(RunTerminalNotification {
            correlation_token: String::new(),
            run_id: *run_id,
            status: RunStatus::Cancelled,
            output_ref: None,
            failure_message_ref: None,
        }),
        _ => None,
    }
}

pub(super) async fn flush_pending_terminal_notifications(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_terminal_notifications));
    for pending in pending {
        let _ = ctx
            .external_workflow(pending.subscription.subscriber_workflow_id, None)
            .signal(AgentSessionWorkflow::run_terminal, pending.notification)
            .await;
    }
    Ok(())
}
