//! One sub-agent delegation, supervised: started on call by the parent
//! session's start-on-call `agent_run` / `agent_spawn` invocation;
//! it creates the child session from the pinned profile, waits for the
//! child's run terminal, resolves the parent's `reply` promise with the
//! result envelope, and closes the child. Cancellation from any direction
//! closes the child. A child's run has no time limit unless its grant sets one;
//! a periodic liveness check is what ends the wait on a child whose session
//! workflow stopped. To the parent this is indistinguishable from an
//! environment job; the session workflow carries no sub-agent code.

use crate::workflows::WorkflowContextExt as _;
use std::time::{Duration, UNIX_EPOCH};

use futures::{FutureExt, pin_mut, select_biased};
use temporalio_common::data_converters::RawValue;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    AgentSessionWorkflow, SubagentChildRef, SubagentCloseActivityRequest, SubagentExecutionInput,
    SubagentExecutionPhase, SubagentExecutionResume, SubagentExecutionSnapshot,
    SubagentLivenessActivityRequest, SubagentLivenessActivityResult,
    SubagentPrepareActivityRequest, SubagentPrepareActivityResult, SubagentResolveActivityRequest,
    SubagentTerminal, WorkflowActivities, WorkflowToolRecoveryResult,
    recovery_poll_activity_options, subagent_activity_options,
};

/// The child session of one execution. Derived from the execution id so a retried
/// prepare finds its own reservation, and so the workflow can release that
/// reservation when prepare never reports back.
pub fn subagent_child_session_id(execution_id: &str) -> harness::SessionId {
    harness::SessionId::new(format!("agent_{}", execution_digest_suffix(execution_id)))
}

/// The child run's submission, derived the same way so a retried start is one run.
pub fn subagent_submission_id(execution_id: &str) -> harness::SubmissionId {
    harness::SubmissionId::new(format!(
        "subagent_run_{}",
        execution_digest_suffix(execution_id)
    ))
}

fn execution_digest_suffix(execution_id: &str) -> String {
    let digest = harness::BlobRef::from_bytes(execution_id.as_bytes());
    digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str())
        .chars()
        .take(32)
        .collect()
}

#[workflow]
#[derive(Default)]
pub struct SubagentExecutionWorkflow {
    snapshot: SubagentExecutionSnapshot,
    /// Identity fixed at start: which promise this execution resolves and
    /// which holder to tell.
    reply_promise_id: Option<harness::PromiseId>,
    holder_workflow_id: Option<String>,
    invocation_id: Option<harness::WorkflowToolInvocationId>,
    universe_id: Option<uuid::Uuid>,
    pending_terminal: Option<SubagentTerminal>,
    holder_cancelled: bool,
    nudged: bool,
    /// What arrived before `run` set the identity it is matched against. A
    /// rollover makes this real: a terminal signalled just after one lands in the
    /// new run's first activation, ahead of `run`, and must not be lost.
    early_emissions: Vec<harness::EmissionEnvelope>,
}

#[workflow_methods]
impl SubagentExecutionWorkflow {
    #[run(name = "SubagentExecutionWorkflow")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: SubagentExecutionInput,
    ) -> WorkflowResult<()> {
        let (start, resumed) = match input {
            SubagentExecutionInput::Start(start) => (start, None),
            SubagentExecutionInput::Resume(resume) => {
                let SubagentExecutionResume {
                    start,
                    child,
                    deadline_at_ms,
                    wait,
                } = resume;
                (start, Some((child, deadline_at_ms, wait)))
            }
        };
        if ctx.workflow_id() != start.execution_id
            || start.universe_id != start.invocation.session_universe_id
        {
            return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                "subagent execution identity is invalid: workflow id or universe mismatch"
            ))
            .into());
        }
        let Some(reply_promise_id) = start
            .invocation
            .completion_promises
            .as_ref()
            .and_then(|promises| promises.get(harness::REPLY_COMPLETION_KEY))
            .cloned()
        else {
            return Err(temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                "subagent execution invocation is missing its reply completion promise"
            ))
            .into());
        };
        ctx.state_mut(|state| {
            state.reply_promise_id = Some(reply_promise_id.clone());
            state.holder_workflow_id = Some(start.holder_workflow_id.clone());
            state.invocation_id = Some(start.invocation.invocation_id.clone());
            state.universe_id = Some(start.universe_id);
            state.snapshot.phase = SubagentExecutionPhase::Preparing;
            if let Some((child, ..)) = &resumed {
                state.snapshot.child = Some(child.clone());
                state.snapshot.phase = SubagentExecutionPhase::Running;
            }
            for envelope in std::mem::take(&mut state.early_emissions) {
                state.apply_emission(envelope);
            }
        });

        // A. prepare: validate the pinned grant, reserve the tree slot,
        // create the child from the pinned profile, start its run. A rollover
        // has done all of this already.
        let (child, deadline_at_ms, wait_policy) = match resumed {
            Some(resumed) => resumed,
            None => {
                let prepared = ctx
                    .execute_activity(
                        WorkflowActivities::subagent_prepare,
                        SubagentPrepareActivityRequest {
                            start: start.clone(),
                        },
                        subagent_activity_options(),
                    )
                    .await;
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    // Failing here frees the parent through its recovery poll, which
                    // reads this message. Whatever prepare reserved or opened before
                    // it failed must not outlive it.
                    Err(error) => {
                        let child = subagent_child_session_id(&start.execution_id);
                        close_child(ctx, start.universe_id, child.as_str()).await;
                        return Err(execution_failure("prepare", &error));
                    }
                };
                match prepared {
                    SubagentPrepareActivityResult::Prepared {
                        child,
                        run_timeout_ms,
                        wait,
                    } => {
                        let deadline_at_ms = deadline_at_ms(workflow_now_ms(ctx), run_timeout_ms);
                        ctx.state_mut(|state| {
                            state.snapshot.child = Some(child.clone());
                            state.snapshot.phase = SubagentExecutionPhase::Running;
                        });
                        (child, deadline_at_ms, wait)
                    }
                    SubagentPrepareActivityResult::Rejected { error_ref } => {
                        let resolution = harness::PromiseResolution::Failed {
                            error_ref: Some(error_ref),
                        };
                        ctx.state_mut(|state| {
                            state.snapshot.phase = SubagentExecutionPhase::Resolved;
                            state.snapshot.resolution = Some(resolution.clone());
                        });
                        emit_resolution(ctx, start.universe_id, &reply_promise_id, resolution)
                            .await;
                        return Ok(());
                    }
                }
            }
        };

        // B. wait: the child's run terminal, the holder's cancellation, the
        // grant deadline if there is one, the child's session stopping, or
        // Temporal cancellation of this execution. The deadline's one timer
        // outlives every pass through the loop, so nothing that wakes the loop
        // moves it.
        let outcome = {
            let ctx = &*ctx;
            let deadline = async {
                match deadline_at_ms {
                    Some(at_ms) => {
                        let remaining = deadline_remaining(at_ms, workflow_now_ms(ctx));
                        let _ = ctx.timer_with_manual_cancellation(remaining).await;
                    }
                    None => futures::future::pending::<()>().await,
                }
            }
            .fuse();
            let cancelled = ctx.cancelled().fuse();
            pin_mut!(deadline, cancelled);
            loop {
                if let Some(terminal) = ctx.state_mut(|state| state.pending_terminal.take()) {
                    break WaitOutcome::Terminal(terminal);
                }
                if ctx.state(|state| state.holder_cancelled) {
                    break WaitOutcome::HolderCancelled;
                }
                if ctx.continue_as_new_suggested()
                    || ctx.history_length() >= wait_policy.continue_as_new_history_threshold
                {
                    ctx.continue_as_new(
                        SubagentExecutionInput::Resume(SubagentExecutionResume {
                            start: start.clone(),
                            child: child.clone(),
                            deadline_at_ms,
                            wait: wait_policy,
                        }),
                        ContinueAsNewOptions::default(),
                    )?;
                }
                ctx.state_mut(|state| state.nudged = false);
                let wait = ctx.wait_for_state(|state| state.nudged);
                let liveness = async {
                    let _ = ctx
                        .timer_with_manual_cancellation(Duration::from_millis(
                            wait_policy.liveness_check_interval_ms.max(1),
                        ))
                        .await;
                    ctx.execute_activity(
                        WorkflowActivities::subagent_liveness,
                        SubagentLivenessActivityRequest {
                            universe_id: start.universe_id,
                            session_id: child.session_id.clone(),
                        },
                        recovery_poll_activity_options(),
                    )
                    .await
                }
                .fuse();
                pin_mut!(wait, liveness);
                // A child that finished as the deadline fired keeps its output, and a
                // cancel beats the deadline it landed with.
                select_biased! {
                    _ = wait => continue,
                    _ = cancelled => break WaitOutcome::Cancelled,
                    _ = deadline => break WaitOutcome::Terminal(SubagentTerminal::Deadline),
                    checked = liveness => match checked {
                        Ok(SubagentLivenessActivityResult::Stopped { reason }) => {
                            // A terminal that beat the check is the better answer.
                            let terminal = ctx.state_mut(|state| state.pending_terminal.take());
                            break WaitOutcome::Terminal(terminal.unwrap_or(
                                SubagentTerminal::SessionStopped { reason },
                            ));
                        }
                        Ok(SubagentLivenessActivityResult::Running) => continue,
                        // A store or Temporal blip says nothing about the child, so
                        // it never fails the delegation; the next check asks again.
                        Err(_) => continue,
                    },
                }
            }
        };

        match outcome {
            WaitOutcome::Terminal(terminal) => {
                // C. resolve: build the envelope, close the child, tell the
                // holder.
                let resolved = ctx
                    .execute_activity(
                        WorkflowActivities::subagent_resolve,
                        SubagentResolveActivityRequest {
                            universe_id: start.universe_id,
                            child: child.clone(),
                            terminal,
                        },
                        subagent_activity_options(),
                    )
                    .await;
                let resolution = match resolved {
                    Ok(resolution) => resolution,
                    Err(error) => {
                        close_child(ctx, start.universe_id, &child.session_id).await;
                        return Err(execution_failure("resolve", &error));
                    }
                };
                ctx.state_mut(|state| {
                    state.snapshot.phase = SubagentExecutionPhase::Resolved;
                    state.snapshot.resolution = Some(resolution.clone());
                });
                emit_resolution(ctx, start.universe_id, &reply_promise_id, resolution).await;
                // Resolve's own close gets one try, so that it cannot hold up the
                // answer. A child it could not close would keep its delegation slot
                // for good, so this one retries, and finds nothing to do otherwise.
                close_child(ctx, start.universe_id, &child.session_id).await;
                Ok(())
            }
            WaitOutcome::HolderCancelled => {
                // The parent already marked the promise cancelled; only the
                // child needs closing.
                close_child(ctx, start.universe_id, &child.session_id).await;
                ctx.state_mut(|state| state.snapshot.phase = SubagentExecutionPhase::Cancelled);
                Ok(())
            }
            WaitOutcome::Cancelled => {
                close_child(ctx, start.universe_id, &child.session_id).await;
                ctx.state_mut(|state| state.snapshot.phase = SubagentExecutionPhase::Cancelled);
                Err(temporalio_sdk::WorkflowTermination::cancelled())
            }
        }
    }

    /// The child's `run_terminal` (its notify intent names this execution
    /// as holder) and the holder's `invocation_cancellation`.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(&mut self, ctx: &mut SyncWorkflowContext<Self>, emission: RawValue) {
        let Ok(envelope) = crate::workflows::receive_emission(ctx, &emission) else {
            return;
        };
        if self.reply_promise_id.is_none() {
            self.early_emissions.push(envelope);
            return;
        }
        self.apply_emission(envelope);
    }

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> SubagentExecutionSnapshot {
        self.snapshot.clone()
    }

    #[query(name = "workflow_tool_recovery")]
    pub fn workflow_tool_recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        recovery_result(&self.snapshot)
    }
}

impl SubagentExecutionWorkflow {
    fn apply_emission(&mut self, envelope: harness::EmissionEnvelope) {
        let identity = SignalIdentity {
            reply_promise_id: self.reply_promise_id.as_ref(),
            holder_workflow_id: self.holder_workflow_id.as_deref(),
            invocation_id: self.invocation_id.as_ref(),
            child: self.snapshot.child.as_ref(),
        };
        match classify_emission(&identity, envelope) {
            Some(SignalEffect::Terminal(terminal)) if self.pending_terminal.is_none() => {
                self.pending_terminal = Some(terminal);
                self.nudged = true;
            }
            Some(SignalEffect::HolderCancelled) => {
                self.holder_cancelled = true;
                self.nudged = true;
            }
            Some(SignalEffect::Terminal(_)) | None => {}
        }
    }
}

/// The instant a set grant deadline falls on, fixed once when the wait begins.
fn deadline_at_ms(now_ms: u64, run_timeout_ms: Option<u64>) -> Option<u64> {
    run_timeout_ms.map(|timeout_ms| now_ms.saturating_add(timeout_ms.max(1)))
}

/// What is left of a deadline at `now_ms`; one already past fires at once.
fn deadline_remaining(deadline_at_ms: u64, now_ms: u64) -> Duration {
    Duration::from_millis(deadline_at_ms.saturating_sub(now_ms).max(1))
}

fn workflow_now_ms(ctx: &WorkflowContext<SubagentExecutionWorkflow>) -> u64 {
    ctx.workflow_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// What the signal handler matches incoming emissions against. Everything
/// but `child` is fixed at start; `child` is known only once the prepare
/// activity's result has been recorded.
pub(crate) struct SignalIdentity<'a> {
    pub reply_promise_id: Option<&'a harness::PromiseId>,
    pub holder_workflow_id: Option<&'a str>,
    pub invocation_id: Option<&'a harness::WorkflowToolInvocationId>,
    pub child: Option<&'a SubagentChildRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignalEffect {
    Terminal(SubagentTerminal),
    HolderCancelled,
}

/// Pure acceptance rule for `deliver_emission`; `None` means the envelope
/// is not addressed to this execution and is dropped.
pub(crate) fn classify_emission(
    identity: &SignalIdentity<'_>,
    envelope: harness::EmissionEnvelope,
) -> Option<SignalEffect> {
    match envelope.body {
        harness::EmissionBody::RunTerminal {
            token,
            run_id,
            status,
            output,
            failure_message_ref,
        } => {
            // The token is the reply promise id, unique to this invocation,
            // so it identifies the child's run on its own. The child may
            // finish before the prepare activity's result is recorded here,
            // so the terminal must be accepted even when the child ref is
            // not known yet; when it is known, the producer must be that
            // child.
            let expected_token = identity.reply_promise_id.map(|promise| promise.as_str());
            let from_child = match (&envelope.producer, identity.child) {
                (harness::EmissionProducer::Session { session_id, .. }, Some(child)) => {
                    session_id.as_str() == child.session_id && run_id.as_u64() == child.run_id
                }
                (harness::EmissionProducer::Session { .. }, None) => true,
                (harness::EmissionProducer::Workflow { .. }, _) => false,
            };
            (expected_token == Some(token.as_str()) && from_child).then_some(
                SignalEffect::Terminal(SubagentTerminal::Run {
                    status,
                    output,
                    failure_message_ref,
                }),
            )
        }
        harness::EmissionBody::InvocationCancellation {
            invocation_id,
            completion_key,
            ..
        } => {
            let from_holder = match &envelope.producer {
                harness::EmissionProducer::Session {
                    universe_id,
                    session_id,
                    ..
                } => {
                    identity.holder_workflow_id
                        == Some(crate::compose_workflow_id(*universe_id, session_id).as_str())
                }
                harness::EmissionProducer::Workflow { .. } => false,
            };
            (from_holder
                && identity.invocation_id == Some(&invocation_id)
                && completion_key == harness::REPLY_COMPLETION_KEY)
                .then_some(SignalEffect::HolderCancelled)
        }
        harness::EmissionBody::SourceResolution { .. }
        | harness::EmissionBody::ToolInvocation { .. } => None,
    }
}

/// The holder-side recovery view: the `reply` resolution once produced.
pub(crate) fn recovery_result(snapshot: &SubagentExecutionSnapshot) -> WorkflowToolRecoveryResult {
    let mut resolutions = std::collections::BTreeMap::new();
    if let Some(resolution) = &snapshot.resolution {
        resolutions.insert(harness::REPLY_COMPLETION_KEY.to_owned(), resolution.clone());
    }
    WorkflowToolRecoveryResult { resolutions }
}

enum WaitOutcome {
    Terminal(SubagentTerminal),
    HolderCancelled,
    Cancelled,
}

/// An activity failure's own text only says that the task failed, and the parent
/// reads what is written here.
fn execution_failure(
    step: &str,
    error: &temporalio_sdk::ActivityExecutionError,
) -> temporalio_sdk::WorkflowTermination {
    let message = crate::workflows::session::activity_failure_message(error);
    temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!("sub-agent {step} failed: {message}"))
        .into()
}

async fn emit_resolution(
    ctx: &mut WorkflowContext<SubagentExecutionWorkflow>,
    universe_id: uuid::Uuid,
    reply_promise_id: &harness::PromiseId,
    resolution: harness::PromiseResolution,
) {
    let Some(holder) = ctx.state(|state| state.holder_workflow_id.clone()) else {
        return;
    };
    let envelope = harness::EmissionEnvelope::source_resolution(
        universe_id,
        ctx.workflow_id().to_owned(),
        &holder,
        reply_promise_id.clone(),
        resolution,
    );
    let _ = ctx
        .external_workflow(holder, None)
        .signal(
            AgentSessionWorkflow::deliver_emission,
            crate::workflows::emission_signal_input(&envelope),
            crate::workflows::signal_options(),
        )
        .await;
}

/// Best effort: the activity logs a close it could not finish, and nothing here
/// can do better with that.
async fn close_child(
    ctx: &mut WorkflowContext<SubagentExecutionWorkflow>,
    universe_id: uuid::Uuid,
    session_id: &str,
) {
    let _ = ctx
        .execute_activity(
            WorkflowActivities::subagent_close,
            SubagentCloseActivityRequest {
                universe_id,
                session_id: session_id.to_owned(),
            },
            subagent_activity_options(),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use harness::{
        BlobRef, EmissionEnvelope, EventSeq, PromiseId, PromiseResolution, REPLY_COMPLETION_KEY,
        RunId, RunStatus, SessionId, WorkflowToolInvocationId,
    };

    use super::*;

    const UNIVERSE: uuid::Uuid = uuid::Uuid::from_u128(7);

    fn parent() -> SessionId {
        SessionId::new("parent-session")
    }

    fn holder_workflow_id() -> String {
        crate::compose_workflow_id(UNIVERSE, &parent())
    }

    fn reply_promise() -> PromiseId {
        PromiseId::new("promise_1")
    }

    fn invocation_id() -> WorkflowToolInvocationId {
        WorkflowToolInvocationId::new(format!("wti:sha256:{}", "a".repeat(64)))
    }

    fn child() -> SubagentChildRef {
        SubagentChildRef {
            session_id: "agent_child".to_owned(),
            run_id: 1,
            agent_profile_id: "reviewer".to_owned(),
        }
    }

    fn run_terminal(session_id: &str, run_id: u64, token: &str) -> EmissionEnvelope {
        EmissionEnvelope::run_terminal(
            UNIVERSE,
            SessionId::new(session_id),
            EventSeq::new(9),
            token.to_owned(),
            RunId::new(run_id),
            RunStatus::Completed,
            Some(harness::ContentRef::text(BlobRef::from_bytes(b"\"done\""))),
            None,
        )
    }

    fn cancellation(
        session_id: &str,
        invocation_id: WorkflowToolInvocationId,
        key: &str,
    ) -> EmissionEnvelope {
        EmissionEnvelope::invocation_cancellation(
            UNIVERSE,
            SessionId::new(session_id),
            EventSeq::new(10),
            invocation_id,
            key.to_owned(),
            reply_promise(),
        )
    }

    fn classify(
        child: Option<&SubagentChildRef>,
        envelope: EmissionEnvelope,
    ) -> Option<SignalEffect> {
        let reply = reply_promise();
        let holder = holder_workflow_id();
        let invocation = invocation_id();
        classify_emission(
            &SignalIdentity {
                reply_promise_id: Some(&reply),
                holder_workflow_id: Some(holder.as_str()),
                invocation_id: Some(&invocation),
                child,
            },
            envelope,
        )
    }

    fn expected_terminal() -> SignalEffect {
        SignalEffect::Terminal(SubagentTerminal::Run {
            status: RunStatus::Completed,
            output: Some(harness::ContentRef::text(BlobRef::from_bytes(b"\"done\""))),
            failure_message_ref: None,
        })
    }

    #[test]
    fn run_terminal_is_accepted_on_the_reply_token_before_the_child_is_known() {
        let effect = classify(
            None,
            run_terminal("agent_child", 1, reply_promise().as_str()),
        );
        assert_eq!(effect, Some(expected_terminal()));
    }

    #[test]
    fn run_terminal_requires_the_known_child_session_and_run() {
        let known = child();
        assert_eq!(
            classify(
                Some(&known),
                run_terminal("agent_child", 1, reply_promise().as_str())
            ),
            Some(expected_terminal())
        );
        assert_eq!(
            classify(
                Some(&known),
                run_terminal("agent_other", 1, reply_promise().as_str())
            ),
            None,
            "another session's terminal must be dropped"
        );
        assert_eq!(
            classify(
                Some(&known),
                run_terminal("agent_child", 2, reply_promise().as_str())
            ),
            None,
            "another run of the child must be dropped"
        );
    }

    #[test]
    fn run_terminal_with_a_foreign_token_or_workflow_producer_is_dropped() {
        assert_eq!(
            classify(None, run_terminal("agent_child", 1, "some-other-token")),
            None
        );
        let mut from_workflow = run_terminal("agent_child", 1, reply_promise().as_str());
        from_workflow.producer = harness::EmissionProducer::Workflow {
            universe_id: UNIVERSE,
            workflow_id: "wte:other".to_owned(),
        };
        assert_eq!(classify(None, from_workflow), None);
    }

    #[test]
    fn holder_cancellation_of_the_reply_key_is_accepted_only_from_the_holder() {
        assert_eq!(
            classify(
                None,
                cancellation("parent-session", invocation_id(), REPLY_COMPLETION_KEY)
            ),
            Some(SignalEffect::HolderCancelled)
        );
        assert_eq!(
            classify(
                None,
                cancellation("other-session", invocation_id(), REPLY_COMPLETION_KEY)
            ),
            None,
            "a cancellation from a session that is not the holder must be dropped"
        );
        assert_eq!(
            classify(
                None,
                cancellation("parent-session", invocation_id(), "job-0")
            ),
            None,
            "only the reply key cancels this execution"
        );
        let other_invocation =
            WorkflowToolInvocationId::new(format!("wti:sha256:{}", "b".repeat(64)));
        assert_eq!(
            classify(
                None,
                cancellation("parent-session", other_invocation, REPLY_COMPLETION_KEY)
            ),
            None,
            "another invocation's cancellation must be dropped"
        );
    }

    #[test]
    fn unrelated_emission_bodies_are_dropped() {
        assert_eq!(
            classify(
                None,
                EmissionEnvelope::source_resolution(
                    UNIVERSE,
                    "wte:other".to_owned(),
                    "universe/parent-session",
                    reply_promise(),
                    PromiseResolution::Resolved { payload_ref: None },
                )
            ),
            None
        );
    }

    #[test]
    fn a_set_deadline_is_one_instant_that_later_wakeups_only_count_down_to() {
        assert_eq!(deadline_at_ms(1_000, None), None);
        let at = deadline_at_ms(1_000, Some(5_000)).expect("a set timeout has an instant");
        assert_eq!(at, 6_000);
        assert_eq!(deadline_remaining(at, 1_000), Duration::from_millis(5_000));
        assert_eq!(deadline_remaining(at, 4_000), Duration::from_millis(2_000));
        assert_eq!(deadline_remaining(at, 6_000), Duration::from_millis(1));
        assert_eq!(deadline_remaining(at, 60_000), Duration::from_millis(1));
    }

    #[test]
    fn the_plain_start_arguments_still_decode_and_a_rollover_decodes_as_itself() {
        let start = crate::WorkflowToolStartArgs {
            universe_id: UNIVERSE,
            holder_workflow_id: holder_workflow_id(),
            execution_id: "wte:one".to_owned(),
            invocation: harness::WorkflowToolInvocation {
                invocation_id: invocation_id(),
                workflow_tool_id: harness::WorkflowToolId::new("agent_run"),
                semantic_type: "agent_run".to_owned(),
                schema_revision: 1,
                binding_fingerprint: "fingerprint".to_owned(),
                session_universe_id: UNIVERSE,
                session_id: parent(),
                run_id: RunId::new(1),
                turn_id: harness::TurnId::new(1),
                tool_batch_id: harness::ToolBatchId::new(1),
                tool_call_id: harness::ToolCallId::new("call"),
                arguments_ref: BlobRef::from_bytes(b"{}"),
                execution_context_ref: None,
                completion_promises: None,
            },
        };
        let plain = serde_json::to_value(&start).expect("encode start");
        assert_eq!(
            serde_json::from_value::<SubagentExecutionInput>(plain).expect("decode start"),
            SubagentExecutionInput::Start(start.clone())
        );
        let resume = SubagentExecutionInput::Resume(SubagentExecutionResume {
            start,
            child: child(),
            deadline_at_ms: Some(9),
            wait: crate::SubagentWaitPolicy::default(),
        });
        let encoded = serde_json::to_value(&resume).expect("encode resume");
        assert_eq!(
            serde_json::from_value::<SubagentExecutionInput>(encoded).expect("decode resume"),
            resume
        );
    }

    #[test]
    fn an_emission_held_until_the_identity_is_set_is_still_applied() {
        let mut workflow = SubagentExecutionWorkflow {
            reply_promise_id: Some(reply_promise()),
            holder_workflow_id: Some(holder_workflow_id()),
            invocation_id: Some(invocation_id()),
            ..Default::default()
        };
        workflow.snapshot.child = Some(child());
        workflow.apply_emission(run_terminal("agent_child", 1, reply_promise().as_str()));
        assert!(workflow.nudged);
        assert!(matches!(
            workflow.pending_terminal,
            Some(SubagentTerminal::Run { .. })
        ));
    }

    #[test]
    fn recovery_exposes_the_reply_resolution_once_produced() {
        let mut snapshot = SubagentExecutionSnapshot::default();
        assert!(recovery_result(&snapshot).resolutions.is_empty());
        let resolution = PromiseResolution::Failed {
            error_ref: Some(BlobRef::from_bytes(b"{\"error\":\"deadline\"}")),
        };
        snapshot.phase = SubagentExecutionPhase::Resolved;
        snapshot.resolution = Some(resolution.clone());
        let recovery = recovery_result(&snapshot);
        assert_eq!(recovery.resolutions.len(), 1);
        assert_eq!(
            recovery.resolutions.get(REPLY_COMPLETION_KEY),
            Some(&resolution)
        );
    }
}
