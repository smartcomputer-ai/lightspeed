use super::*;

/// Recent refusals a waiter can still be polling for. A waiter looks its own up by
/// correlation token, so the oldest can go.
const MAX_RETAINED_ADMISSION_FAILURES: usize = 256;

impl AgentSessionWorkflow {
    pub(super) fn record_admission_failure(&mut self, failure: AgentAdmissionFailure) {
        self.admission_failures.push(failure);
        let excess = self
            .admission_failures
            .len()
            .saturating_sub(MAX_RETAINED_ADMISSION_FAILURES);
        self.admission_failures.drain(..excess);
    }

    /// Submissions the workflow has yet to admit or refuse.
    fn pending_submission_ids(&self) -> Vec<SubmissionId> {
        self.pending_admissions
            .iter()
            .filter_map(SessionAdmission::core)
            .chain(
                self.run_preparation
                    .iter()
                    .map(|preparation| &preparation.admission),
            )
            .filter_map(|admission| drive::command_submission_id(&admission.command))
            .collect()
    }

    pub fn queue_admission(&mut self, admission: AgentAdmission) {
        self.pending_admissions
            .push(SessionAdmission::Core(admission));
    }

    /// Inbound push delivery converges on ordinary promise resolution
    /// admission. The receiver's promise registry remains the semantic
    /// idempotency boundary for duplicate transport deliveries. An emission
    /// this session will not take comes back as a warning for the caller to
    /// record; it does not disturb the session.
    pub fn queue_emission(
        &mut self,
        universe_id: uuid::Uuid,
        envelope: EmissionEnvelope,
    ) -> Result<(), String> {
        if envelope.producer.universe_id() != universe_id {
            return Err(format!(
                "cross-universe emission rejected: receiver_universe={} producer_universe={}",
                universe_id,
                envelope.producer.universe_id()
            ));
        }
        match envelope.body {
            // A receiving workflow interprets the typed run output and then
            // sends an authorized SourceResolution. Dropping its encoding
            // here would expose provider JSON as an ordinary tool result.
            harness::EmissionBody::RunTerminal { .. } => Err(
                "session workflow requires source resolutions, not run-terminal notifications"
                    .to_owned(),
            ),
            harness::EmissionBody::SourceResolution {
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
                Ok(())
            }
            harness::EmissionBody::ToolInvocation { invocation, .. } => Err(format!(
                "session workflow cannot receive workflow tool invocation {}",
                invocation.invocation_id
            )),
            harness::EmissionBody::InvocationCancellation { invocation_id, .. } => Err(format!(
                "session workflow cannot receive workflow tool cancellation {invocation_id}"
            )),
        }
    }

    /// Keep an advisory fact on the status snapshot and in the log. A warning
    /// says something was refused or left undone while the session carried
    /// on, so no gateway path treats it as an error.
    pub(super) fn record_warning(&mut self, replaying: bool, warning: String) {
        if !replaying {
            tracing::warn!(
                target: "temporal_workflow",
                event = "session_workflow_warning",
                session_id = self.session_id.as_ref().map(ToString::to_string).unwrap_or_default(),
                warning = warning.as_str(),
                "session workflow warning"
            );
        }
        self.last_warning = Some(warning);
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
            if let CoreAgentEvent::WorkflowTool(harness::WorkflowToolEvent::Invoked {
                invocation,
            }) = &entry.event
            {
                let Some(binding) = self
                    .core_state
                    .workflow_tools
                    .bindings
                    .get(&invocation.workflow_tool_id)
                else {
                    anyhow::bail!(
                        "invoked workflow tool invocation {} has no durable binding",
                        invocation.invocation_id
                    );
                };
                match &binding.target {
                    harness::WorkflowToolTarget::Bound {
                        receiver,
                        dispatch: harness::BoundWorkflowToolDispatch::Push,
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
                    harness::WorkflowToolTarget::Bound {
                        dispatch: harness::BoundWorkflowToolDispatch::Pull,
                        ..
                    } => {}
                    harness::WorkflowToolTarget::Start { .. } => {
                        anyhow::bail!(
                            "invoked workflow tool invocation {} has a start-target binding",
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
                .finished
                .iter()
                .find(|record| record.run_id == run_id)
            else {
                continue;
            };
            let emissions =
                run_terminal_emissions(universe_id, &session_id, entry.position.seq, record);
            self.pending_emissions.extend(emissions);
        }
        Ok(())
    }

    /// A receiver hears when the session stops waiting for it: on a cancel, and when
    /// the hard deadline fails its promise. The deadline's own error ref tells that
    /// failure from a producer's report, which is not echoed back.
    pub(super) fn queue_promise_cancellation_for_entry(&mut self, entry: &CoreAgentEntry) {
        let promise_id = match &entry.event {
            CoreAgentEvent::Promise(harness::PromiseEvent::Cancelled { promise_id }) => promise_id,
            CoreAgentEvent::Promise(harness::PromiseEvent::Failed {
                promise_id,
                error_ref: Some(error_ref),
            }) if *error_ref == harness::promise_deadline_exceeded_ref() => promise_id,
            _ => return,
        };
        let Some(promise) = self.core_state.promises.promises.get(promise_id) else {
            return;
        };
        self.pending_promise_cancellations
            .push(PendingPromiseCancellation {
                promise_id: promise_id.as_str().to_owned(),
                source: promise.source.clone(),
                log_seq: entry.position.seq.as_u64(),
            });
    }

    /// When this entry ends the last pending keyed promise of a start-on-call
    /// invocation, queue the cancel of its exact execution. Runs once per entry, right
    /// after that entry is applied, because a later entry of the same append may prune
    /// the records read here.
    pub(super) fn queue_execution_cancel_for_entry(&mut self, entry: &CoreAgentEntry) {
        let CoreAgentEvent::Promise(
            harness::PromiseEvent::Resolved { promise_id, .. }
            | harness::PromiseEvent::Failed { promise_id, .. }
            | harness::PromiseEvent::Cancelled { promise_id },
        ) = &entry.event
        else {
            return;
        };
        let promises = &self.core_state.promises.promises;
        let Some(harness::PromiseSource::Workflow { invocation_id, .. }) =
            promises.get(promise_id).map(|promise| &promise.source)
        else {
            return;
        };
        let Ok(invocation_id) = harness::WorkflowToolInvocationId::try_new(invocation_id.clone())
        else {
            return;
        };
        let tools = &self.core_state.workflow_tools;
        let Some(invocation) = tools.start_requests.get(&invocation_id) else {
            return;
        };
        let all_terminal = invocation
            .completion_promises
            .iter()
            .flat_map(|keyed| keyed.values())
            .all(|id| promises.get(id).is_none_or(|p| p.status.is_terminal()));
        let start = match tools
            .bindings
            .get(&invocation.workflow_tool_id)
            .map(|b| &b.target)
        {
            Some(harness::WorkflowToolTarget::Start { start }) if all_terminal => start,
            _ => return,
        };
        let execution_id =
            harness::workflow_tool_execution_id(&invocation_id, &start.recipe_fingerprint);
        self.pending_execution_cancels.insert(execution_id);
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
            pending_submission_ids: self.pending_submission_ids(),
            pending_tool_batch_unparks: self.pending_tool_batch_unparks.len(),
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
            finished_runs: self
                .core_state
                .runs
                .finished
                .iter()
                .map(|run| AgentFinishedRunSummary {
                    run_id: run.run_id.as_u64(),
                    status: run.status,
                    submission_id: run.submission_id.clone(),
                    output: run.output.clone(),
                })
                .collect(),
            admission_failures: self.admission_failures.clone(),
            fatal_error: self.fatal_error.clone(),
            last_warning: self.last_warning.clone(),
            bootstrap_failed: self.bootstrap_failed,
        }
    }
}

/// One notification per intent on a run's terminal record.
pub fn run_terminal_emissions(
    universe_id: uuid::Uuid,
    session_id: &SessionId,
    log_seq: harness::EventSeq,
    record: &harness::RunRecord,
) -> Vec<PendingEmission> {
    record
        .notify_on_terminal
        .iter()
        .map(|intent| {
            PendingEmission::immediate(
                intent.holder_workflow_id.clone(),
                EmissionEnvelope::run_terminal(
                    universe_id,
                    session_id.clone(),
                    log_seq,
                    intent.token.clone(),
                    record.run_id,
                    record.status,
                    record.output.clone(),
                    record
                        .failure
                        .as_ref()
                        .and_then(|failure| failure.message_ref.clone()),
                ),
            )
        })
        .collect()
}

fn terminal_run_id_for_event(event: &CoreAgentEvent) -> Option<harness::RunId> {
    match event {
        CoreAgentEvent::Run(
            RunEvent::Completed { run_id, .. }
            | RunEvent::Failed { run_id, .. }
            | RunEvent::Cancelled { run_id }
            | RunEvent::ForceCancelled { run_id }
            | RunEvent::QueuedCancelled { run_id, .. },
        ) => Some(*run_id),
        _ => None,
    }
}

/// Bounded independent retry for pushed tool-invocation delivery.
/// Other emission bodies get one attempt and drop if the target is not running.
const MAX_TOOL_INVOCATION_DELIVERY_ATTEMPTS: u32 = 5;
const TOOL_INVOCATION_RETRY_BACKOFF_MS: u64 = 2_000;

/// Deliver queued facts by signalling each receiver workflow's fixed
/// `deliver_emission` handler. Signals to a running workflow are durable; for
/// run-terminal and source-resolution bodies a target that is not running drops
/// the entry, and nothing sends it again. A controller reconciles a missed
/// terminal through the run read. Pushed tool invocations instead retry each envelope
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
                crate::workflows::emission_signal_input(&pending.envelope),
                crate::workflows::signal_options(),
            )
            .await
            .is_ok();
        if delivered {
            continue;
        }
        if !matches!(
            pending.envelope.body,
            harness::EmissionBody::ToolInvocation { .. }
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
    let harness::EmissionBody::ToolInvocation { invocation, .. } = &pending.envelope.body else {
        return Ok(());
    };
    let message = format!(
        "workflow tool invocation {} could not be delivered to receiver {} after {} attempts",
        invocation.invocation_id, pending.receiver_workflow_id, pending.attempts
    );
    // An accepted invocation is finished once committed, so its record can be gone
    // by the time delivery gives up. No run waits on it, but the receiver never heard.
    let replaying = ctx.is_replaying();
    if ctx.state_mut(|state| {
        let gone = !state
            .core_state
            .workflow_tools
            .bound_invocations
            .contains_key(&invocation.invocation_id);
        if gone {
            state.record_warning(replaying, message.clone());
        }
        gone
    }) {
        return Ok(());
    }
    let error_ref = ctx
        .execute_activity(
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
            ctx.state_mut(|state| state.record_admission_failure(failure));
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

/// Whether a flush at `now_ms` would attempt anything. Pass 0 for the entries that
/// are not waiting out a retry; those that are wake through their own timer.
pub(super) fn has_due_emissions(state: &AgentSessionWorkflow, now_ms: u64) -> bool {
    state
        .pending_emissions
        .iter()
        .any(|pending| pending.next_attempt_at_ms <= now_ms)
}
