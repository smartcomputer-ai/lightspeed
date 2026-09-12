use super::*;

impl AgentSessionWorkflow {
    pub fn queue_admission(&mut self, admission: AgentAdmission) {
        self.pending_admissions
            .push(SessionAdmission::Core(admission));
    }

    /// Inbound push delivery converges on ordinary promise resolution
    /// admission. The receiver's promise registry remains the semantic
    /// idempotency boundary for duplicate transport deliveries.
    pub fn queue_emission(&mut self, universe_id: uuid::Uuid, envelope: EmissionEnvelope) {
        if envelope.producer.universe_id() != universe_id {
            self.last_error = Some(format!(
                "cross-universe emission rejected: receiver_universe={} producer_universe={}",
                universe_id,
                envelope.producer.universe_id()
            ));
            return;
        }
        match envelope.body {
            engine::EmissionBody::RunTerminal { .. } => {
                // A receiving workflow interprets the typed run output and then
                // sends an authorized SourceResolution. Dropping its encoding
                // here would expose provider JSON as an ordinary tool result.
                self.last_error = Some(
                    "session workflow requires source resolutions, not run-terminal notifications"
                        .to_owned(),
                );
            }
            engine::EmissionBody::SourceResolution {
                promise_id,
                resolution,
            } => {
                // Producer authorization and optional reply-schema
                // validation need core state and activities; defer to the
                // main loop instead of admitting directly from the signal.
                self.pending_source_resolutions
                    .push(PendingSourceResolution {
                        promise_id,
                        resolution,
                        producer: envelope.producer,
                    });
            }
            engine::EmissionBody::ToolInvocation { invocation, .. } => {
                self.last_error = Some(format!(
                    "session workflow cannot receive workflow tool invocation {}",
                    invocation.invocation_id
                ));
            }
            engine::EmissionBody::InvocationCancellation { invocation_id, .. } => {
                self.last_error = Some(format!(
                    "session workflow cannot receive workflow tool cancellation {invocation_id}"
                ));
            }
        }
    }

    /// Outbound push delivery: when a run carrying notify-intents reaches a
    /// terminal state, queue one notification per intent. Intents live on
    /// the run record in core state (the edge event is the subscription), so
    /// this consults the just-applied record — no subscription table.
    pub(super) fn queue_emissions_for_entries(
        &mut self,
        entries: &[CoreAgentEntry],
    ) -> anyhow::Result<()> {
        let universe_id = self
            .universe_id
            .ok_or_else(|| anyhow::anyhow!("initialized session is missing universe id"))?;
        let session_id = self
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("initialized session is missing session id"))?;
        for entry in entries {
            // Bound dispatch is independent of caller completion. Every
            // pushed invocation enters the same durable Temporal delivery
            // spine after its admitting append commits; pull invocations are
            // consumed later through the receiver-authorized log read.
            if let CoreAgentEvent::WorkflowTool(engine::WorkflowToolEvent::Emitted { invocation }) =
                &entry.event
            {
                let Some(binding) = self
                    .core_state
                    .workflow_tools
                    .bindings
                    .get(&invocation.tool_id)
                else {
                    anyhow::bail!(
                        "emitted workflow tool invocation {} has no durable binding",
                        invocation.invocation_id
                    );
                };
                match &binding.target {
                    engine::WorkflowToolTarget::Bound {
                        receiver,
                        dispatch: engine::BoundWorkflowToolDispatch::Push,
                    } => {
                        self.pending_emissions.push(PendingEmission::immediate(
                            receiver.workflow_id.clone(),
                            EmissionEnvelope::tool_invocation(
                                universe_id,
                                session_id.clone(),
                                entry.position.seq,
                                invocation.clone(),
                                crate::compose_workflow_id(universe_id, &session_id),
                            ),
                        ));
                        continue;
                    }
                    engine::WorkflowToolTarget::Bound {
                        dispatch: engine::BoundWorkflowToolDispatch::Pull,
                        ..
                    } => {}
                    engine::WorkflowToolTarget::Start { .. } => {
                        anyhow::bail!(
                            "emitted workflow tool invocation {} has a start-target binding",
                            invocation.invocation_id
                        );
                    }
                };
            }
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
                self.pending_emissions.push(PendingEmission::immediate(
                    intent.holder_workflow_id.clone(),
                    EmissionEnvelope::run_terminal(
                        universe_id,
                        session_id.clone(),
                        entry.position.seq,
                        intent.token.clone(),
                        run_id,
                        record.status,
                        record.output.clone(),
                        record
                            .failure
                            .as_ref()
                            .and_then(|failure| failure.message_ref.clone()),
                    ),
                ));
            }
        }
        Ok(())
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
                    log_seq: entry.position.seq.as_u64(),
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
            ready: self.ready,
            setup_error: self.setup_error.clone(),
            pending_admissions: self.pending_admissions.len()
                + usize::from(self.run_preparation.is_some()),
            pending_tool_batch_resumes: self.pending_tool_batch_resumes.len(),
            active_waits: usize::from(awaits::parked_tool_batch(&self.core_state).is_some())
                + self.promise_source_polls.len(),
            pending_emissions: self.pending_emissions.len(),
            active_run: self
                .core_state
                .runs
                .active
                .as_ref()
                .map(|run| AgentActiveRunSummary {
                    run_id: run.run_id.as_u64(),
                    status: run.status,
                    submission_id: run.submission_id.clone(),
                    output: run.output.clone(),
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
                    output: run.output.clone(),
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

/// Bounded independent retry for pushed tool-invocation delivery.
/// Other emission bodies keep the legacy single-attempt, drop-on-missing
/// semantics.
const MAX_TOOL_INVOCATION_DELIVERY_ATTEMPTS: u32 = 5;
const TOOL_INVOCATION_RETRY_BACKOFF_MS: u64 = 2_000;

/// Deliver queued facts by signalling each receiver workflow's fixed
/// `deliver_emission` handler. Signals to an existing workflow id are durable;
/// for run-terminal and source-resolution bodies a missing target drops the
/// entry (its holder is gone — the reaper's upward sweep covers that
/// direction). Pushed tool invocations instead retry each envelope
/// independently with deterministic bounded backoff. Exhausted delivery
/// appends terminal `DeliveryFailed`; promise-bearing invocations also fail
/// their still-pending completion promises in the same append, while
/// Accepted calls remain successfully completed. The queue gates
/// continue-as-new, so in-flight deliveries never need reconstruction.
pub(super) async fn flush_pending_emissions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_emissions));
    let mut retained = Vec::new();
    for mut pending in pending {
        if pending.next_attempt_at_ms > now {
            retained.push(pending);
            continue;
        }
        let delivered = ctx
            .external_workflow(pending.receiver_workflow_id.clone(), None)
            .signal(
                AgentSessionWorkflow::deliver_emission,
                pending.envelope.clone(),
            )
            .await
            .is_ok();
        if delivered {
            continue;
        }
        if !matches!(
            pending.envelope.body,
            engine::EmissionBody::ToolInvocation { .. }
        ) {
            continue;
        }
        pending.attempts = pending.attempts.saturating_add(1);
        if pending.attempts >= MAX_TOOL_INVOCATION_DELIVERY_ATTEMPTS {
            terminal_tool_invocation_delivery_failure(ctx, &pending).await?;
            continue;
        }
        pending.next_attempt_at_ms =
            now + TOOL_INVOCATION_RETRY_BACKOFF_MS * u64::from(pending.attempts);
        retained.push(pending);
    }
    ctx.state_mut(|state| {
        retained.append(&mut state.pending_emissions);
        state.pending_emissions = retained;
    });
    Ok(())
}

async fn terminal_tool_invocation_delivery_failure(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    pending: &PendingEmission,
) -> anyhow::Result<()> {
    let engine::EmissionBody::ToolInvocation { invocation, .. } = &pending.envelope.body else {
        return Ok(());
    };
    let message = format!(
        "workflow tool invocation {} could not be delivered to receiver {} after {} attempts",
        invocation.invocation_id, pending.receiver_workflow_id, pending.attempts
    );
    let error_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: message.into_bytes(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut drive = drive_from_state(ctx)?;
    match admit_and_append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::FailWorkflowToolDelivery {
            invocation_id: invocation.invocation_id.clone(),
            error_ref,
        },
        None,
    )
    .await?
    {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            ctx.state_mut(|state| state.admission_failures.push(failure));
            Ok(())
        }
    }
}

/// Earliest scheduled retry among backing-off emission entries; entries with
/// `next_attempt_at_ms == 0` are immediate work instead.
pub(super) fn nearest_emission_retry_ms(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .pending_emissions
        .iter()
        .map(|pending| pending.next_attempt_at_ms)
        .filter(|at_ms| *at_ms > 0)
        .min()
}

pub(super) fn has_due_emissions(state: &AgentSessionWorkflow) -> bool {
    state
        .pending_emissions
        .iter()
        .any(|pending| pending.next_attempt_at_ms == 0)
}
