use super::*;

impl AgentSessionWorkflow {
    pub fn queue_admission(&mut self, admission: AgentAdmission) {
        self.pending_admissions.push(admission);
    }

    /// Inbound push delivery: a session we hold a promise on reports the
    /// terminal state of the run behind it. Queued as a normal admission so
    /// the resolution flows through `ResolvePromise` command admission —
    /// the single funnel — where a promise that is already terminal makes
    /// the delivery an idempotent no-op.
    pub fn queue_promise_resolution(&mut self, signal: PromiseResolutionSignal) {
        let resolution = match signal.status {
            RunStatus::Completed => engine::PromiseResolution::Resolved {
                payload_ref: signal.output_ref,
            },
            // A failed or externally-cancelled source is a failed promise
            // for the holder; promise `cancelled` is reserved for the
            // holder's own revocation.
            _ => engine::PromiseResolution::Failed {
                error_ref: signal.failure_message_ref,
            },
        };
        self.pending_admissions.push(AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id: engine::PromiseId::new(signal.token),
                resolution,
            },
            correlation_token: None,
        });
    }

    pub fn queue_promise_source_resolution(&mut self, signal: PromiseSourceResolutionSignal) {
        let resolution = match signal.result {
            engine::PromiseSourceCheckResult::Pending => return,
            engine::PromiseSourceCheckResult::Resolved { payload_ref } => {
                engine::PromiseResolution::Resolved { payload_ref }
            }
            engine::PromiseSourceCheckResult::Failed { error_ref } => {
                engine::PromiseResolution::Failed { error_ref }
            }
        };
        self.pending_admissions.push(AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id: engine::PromiseId::new(signal.promise_id),
                resolution,
            },
            correlation_token: None,
        });
    }

    /// Outbound push delivery: when a run carrying notify-intents reaches a
    /// terminal state, queue one notification per intent. Intents live on
    /// the run record in core state (the edge event is the subscription), so
    /// this consults the just-applied record — no subscription table.
    pub(super) fn queue_promise_notifications_for_entries(&mut self, entries: &[CoreAgentEntry]) {
        for entry in entries {
            let Some(run_id) = terminal_run_id_for_event(&entry.event) else {
                continue;
            };
            let Some(record) = self
                .core_state
                .runs
                .completed
                .iter()
                .find(|record| record.run_id == run_id)
            else {
                continue;
            };
            for intent in &record.notify_on_terminal {
                self.pending_promise_notifications
                    .push(PendingPromiseNotification {
                        holder_workflow_id: intent.holder_workflow_id.clone(),
                        signal: PromiseResolutionSignal {
                            token: intent.token.clone(),
                            status: record.status,
                            output_ref: record.output_ref.clone(),
                            failure_message_ref: record
                                .failure
                                .as_ref()
                                .and_then(|failure| failure.message_ref.clone()),
                        },
                    });
            }
        }
    }

    pub(super) fn queue_promise_cancellations_for_entries(&mut self, entries: &[CoreAgentEntry]) {
        for entry in entries {
            let CoreAgentEvent::Promise(engine::PromiseEvent::Cancelled { promise_id }) =
                &entry.event
            else {
                continue;
            };
            let Some(promise) = self.core_state.promises.promises.get(promise_id) else {
                continue;
            };
            self.pending_promise_cancellations
                .push(PendingPromiseCancellation {
                    promise_id: promise_id.as_str().to_owned(),
                    source: promise.source.clone(),
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
            active_waits: usize::from(awaits::parked_await(&self.core_state).is_some())
                + self.promise_source_polls.len(),
            pending_promise_notifications: self.pending_promise_notifications.len(),
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
            consumed_message_submissions: self
                .core_state
                .runs
                .messages
                .iter()
                .filter_map(|message| {
                    if message.status != engine::MessageStatus::ConsumedByAwait {
                        return None;
                    }
                    Some(AgentMessageSubmissionConsumptionSummary {
                        submission_id: message.submission_id.clone()?,
                        run_id: message.consumed_by_run_id?.as_u64(),
                    })
                })
                .collect(),
            admission_failures: self.admission_failures.clone(),
            last_error: self.last_error.clone(),
            bootstrap_failed: self.bootstrap_failed,
        }
    }
}

fn terminal_run_id_for_event(event: &CoreAgentEvent) -> Option<engine::RunId> {
    match event {
        CoreAgentEvent::Run(
            RunEvent::Completed { run_id, .. }
            | RunEvent::Failed { run_id, .. }
            | RunEvent::Cancelled { run_id }
            | RunEvent::ForceCancelled { run_id }
            | RunEvent::QueuedCancelled { run_id },
        ) => Some(*run_id),
        _ => None,
    }
}

/// Deliver queued notifications by signalling each holder workflow's
/// `resolve_promise` handler. Signals to an existing workflow id are durable;
/// a missing target drops the entry (its holder is gone — the reaper's
/// upward sweep covers that direction). The queue gates continue-as-new, so
/// in-flight deliveries never need reconstruction.
pub(super) async fn flush_pending_promise_notifications(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_promise_notifications));
    for pending in pending {
        let _ = ctx
            .external_workflow(pending.holder_workflow_id, None)
            .signal(AgentSessionWorkflow::resolve_promise, pending.signal)
            .await;
    }
    Ok(())
}
