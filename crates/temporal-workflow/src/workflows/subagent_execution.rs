//! One sub-agent delegation, supervised: started on call by the parent
//! session's start-on-call `agent_run` / `agent_spawn` invocation;
//! it creates the child session from the pinned profile, waits for the
//! child's run terminal, resolves the parent's `reply` promise with the
//! result envelope, and closes the child. Cancellation from any direction
//! closes the child. To the parent this is indistinguishable from an
//! environment job; the session workflow carries no sub-agent code.

use crate::workflows::WorkflowContextExt as _;
use std::time::Duration;

use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult};

use crate::{
    AgentSessionWorkflow, SubagentChildRef, SubagentCloseActivityRequest, SubagentExecutionPhase,
    SubagentExecutionSnapshot, SubagentPrepareActivityRequest, SubagentPrepareActivityResult,
    SubagentResolveActivityRequest, SubagentTerminal, WorkflowActivities,
    WorkflowToolRecoveryResult, WorkflowToolStartArgs, activity_options,
};

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
}

#[workflow_methods]
impl SubagentExecutionWorkflow {
    #[run(name = "SubagentExecutionWorkflow")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        start: WorkflowToolStartArgs,
    ) -> WorkflowResult<()> {
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
        });

        // A. prepare: validate the pinned grant, reserve the tree slot,
        // create the child from the pinned profile, start its run.
        let prepared = ctx
            .execute_activity(
                WorkflowActivities::subagent_prepare,
                SubagentPrepareActivityRequest {
                    start: start.clone(),
                },
                activity_options(),
            )
            .await
            .map_err(|error| {
                temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                    "subagent prepare failed: {error}"
                ))
            })?;
        let (child, deadline_ms) = match prepared {
            SubagentPrepareActivityResult::Prepared { child, deadline_ms } => (child, deadline_ms),
            SubagentPrepareActivityResult::Rejected { error_ref } => {
                let resolution = harness::PromiseResolution::Failed {
                    error_ref: Some(error_ref),
                };
                ctx.state_mut(|state| {
                    state.snapshot.phase = SubagentExecutionPhase::Resolved;
                    state.snapshot.resolution = Some(resolution.clone());
                });
                emit_resolution(ctx, start.universe_id, &reply_promise_id, resolution).await;
                return Ok(());
            }
        };
        ctx.state_mut(|state| {
            state.snapshot.child = Some(child.clone());
            state.snapshot.phase = SubagentExecutionPhase::Running;
        });

        // B. wait: the child's run terminal, the holder's cancellation, the
        // grant deadline, or Temporal cancellation of this execution.
        let outcome = loop {
            if let Some(terminal) = ctx.state_mut(|state| state.pending_terminal.take()) {
                break WaitOutcome::Terminal(terminal);
            }
            if ctx.state(|state| state.holder_cancelled) {
                break WaitOutcome::HolderCancelled;
            }
            ctx.state_mut(|state| state.nudged = false);
            let wait = ctx.wait_for_state(|state| state.nudged);
            let deadline = ctx
                .timer_with_manual_cancellation(Duration::from_millis(deadline_ms.max(1)))
                .fuse();
            let cancelled = ctx.cancelled().fuse();
            pin_mut!(wait, deadline, cancelled);
            select! {
                _ = wait => continue,
                _ = deadline => break WaitOutcome::Terminal(SubagentTerminal::Deadline),
                _ = cancelled => break WaitOutcome::Cancelled,
            }
        };

        match outcome {
            WaitOutcome::Terminal(terminal) => {
                // C. resolve: build the envelope, close the child, tell the
                // holder.
                let resolution = ctx
                    .execute_activity(
                        WorkflowActivities::subagent_resolve,
                        SubagentResolveActivityRequest {
                            universe_id: start.universe_id,
                            child: child.clone(),
                            terminal,
                        },
                        activity_options(),
                    )
                    .await
                    .map_err(|error| {
                        temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                            "subagent resolve failed: {error}"
                        ))
                    })?;
                ctx.state_mut(|state| {
                    state.snapshot.phase = SubagentExecutionPhase::Resolved;
                    state.snapshot.resolution = Some(resolution.clone());
                });
                emit_resolution(ctx, start.universe_id, &reply_promise_id, resolution).await;
                Ok(())
            }
            WaitOutcome::HolderCancelled => {
                // The parent already marked the promise cancelled; only the
                // child needs closing.
                close_child(ctx, start.universe_id, &child).await;
                ctx.state_mut(|state| state.snapshot.phase = SubagentExecutionPhase::Cancelled);
                Ok(())
            }
            WaitOutcome::Cancelled => {
                close_child(ctx, start.universe_id, &child).await;
                ctx.state_mut(|state| state.snapshot.phase = SubagentExecutionPhase::Cancelled);
                Err(temporalio_sdk::WorkflowTermination::cancelled())
            }
        }
    }

    /// The child's `run_terminal` (its notify intent names this execution
    /// as holder) and the holder's `invocation_cancellation`.
    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        envelope: harness::EmissionEnvelope,
    ) {
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

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> SubagentExecutionSnapshot {
        self.snapshot.clone()
    }

    #[query(name = "workflow_tool_recovery")]
    pub fn workflow_tool_recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        recovery_result(&self.snapshot)
    }
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
            envelope,
            crate::workflows::signal_options(),
        )
        .await;
}

async fn close_child(
    ctx: &mut WorkflowContext<SubagentExecutionWorkflow>,
    universe_id: uuid::Uuid,
    child: &SubagentChildRef,
) {
    let _ = ctx
        .execute_activity(
            WorkflowActivities::subagent_close,
            SubagentCloseActivityRequest {
                universe_id,
                session_id: child.session_id.clone(),
            },
            activity_options(),
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
