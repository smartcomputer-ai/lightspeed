//! Durable supervision of one ephemeral JavaScript attempt. Session-owned
//! activities still execute every tool effect; this workflow never replays JS.

use std::time::Duration;

use futures::{FutureExt, select_biased};
use temporalio_common::protos::temporal::api::common::v1::RetryPolicy;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityCancellationType, ActivityCloseTimeouts, ActivityOptions, CancellableFuture,
    SyncWorkflowContext, WorkflowCancellationToken, WorkflowContext, WorkflowContextView,
    WorkflowResult,
};

use crate::{
    AgentSessionWorkflow, CodeExecutionDescriptor, CodeExecutionInterruption, CodeExecutionPhase,
    CodeExecutionSnapshot, CodeExecutionTerminal, CodeFinalizeActivityRequest,
    CodePrepareActivityRequest, CodePrepareActivityResult, WorkflowActivities,
    WorkflowToolRecoveryResult, WorkflowToolStartArgs, workflows::WorkflowContextExt as _,
};

const ACTIVITY_HEARTBEAT: Duration = Duration::from_secs(15);
const ACTIVITY_CANCELLATION_GRACE: Duration = Duration::from_secs(20);

#[workflow]
#[derive(Default)]
pub struct CodeExecutionWorkflow {
    snapshot: CodeExecutionSnapshot,
    start: Option<WorkflowToolStartArgs>,
    holder_cancelled: bool,
}

#[workflow_methods]
impl CodeExecutionWorkflow {
    #[run(name = "CodeExecutionWorkflow")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        start: WorkflowToolStartArgs,
    ) -> WorkflowResult<()> {
        let reply = validate_start(ctx.workflow_id(), &start)
            .map_err(|message| temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(message)))?;
        ctx.state_mut(|state| {
            state.start = Some(start.clone());
            state.snapshot.phase = CodeExecutionPhase::Preparing;
        });

        let mut prepare = ctx.execute_activity(
            WorkflowActivities::code_prepare,
            CodePrepareActivityRequest {
                start: start.clone(),
            },
            preparation_options(),
        );
        let prepared = {
            let holder_cancel = ctx.wait_for_state(|state| state.holder_cancelled);
            let external_cancel = ctx.cancelled().fuse();
            let work = (&mut prepare).fuse();
            futures::pin_mut!(holder_cancel, external_cancel, work);
            select_biased! {
                _ = holder_cancel => Err(CodeExecutionInterruption::HolderCancelled),
                _ = external_cancel => Err(CodeExecutionInterruption::WorkflowCancelled),
                result = work => Ok(result),
            }
        };
        let mut terminal = match prepared {
            Err(reason) => {
                prepare.cancel();
                // A prepare receipt can race with cancellation. Record it if
                // it arrives; finalization also works without it by scope id.
                let grace = ctx
                    .timer_with_manual_cancellation(ACTIVITY_CANCELLATION_GRACE)
                    .fuse();
                let work = prepare.fuse();
                futures::pin_mut!(grace, work);
                select_biased! {
                    result = work => {
                        if let Ok(CodePrepareActivityResult::Prepared { descriptor }) = result {
                            ctx.state_mut(|state| state.snapshot.descriptor = Some(descriptor));
                        }
                    },
                    _ = grace => {},
                }
                CodeExecutionTerminal::Interrupted {
                    reason,
                    result: None,
                }
            }
            Ok(Err(_)) => CodeExecutionTerminal::Interrupted {
                reason: CodeExecutionInterruption::PreparationFailed,
                result: None,
            },
            Ok(Ok(CodePrepareActivityResult::Rejected { error_ref })) => {
                CodeExecutionTerminal::Rejected { error_ref }
            }
            Ok(Ok(CodePrepareActivityResult::Prepared { descriptor })) => {
                if !descriptor_matches_start(&descriptor, &start) {
                    CodeExecutionTerminal::Interrupted {
                        reason: CodeExecutionInterruption::PreparationFailed,
                        result: None,
                    }
                } else {
                    ctx.state_mut(|state| {
                        state.snapshot.descriptor = Some(descriptor.clone());
                        state.snapshot.phase = CodeExecutionPhase::Running;
                    });
                    run_once(ctx, descriptor).await
                }
            }
        };

        ctx.state_mut(|state| {
            state.snapshot.phase = CodeExecutionPhase::Finalizing;
            state.snapshot.terminal = Some(terminal.clone());
        });
        // Detached cancellation is intentional: external workflow cancellation
        // must still close the scope and persist a report of acknowledged work.
        let mut resolution = finalize(ctx, &start, &terminal).await?;
        if let Some(cancelled_terminal) = terminal_after_cancellation(
            &terminal,
            ctx.state(|state| state.holder_cancelled),
            ctx.cancellation_token().is_cancelled(),
        ) {
            // A cancelled code tool wait can finish the script before the holder's
            // cancellation reaches this workflow. If cancellation arrived while
            // finalization was in flight, retain the runner receipt and publish
            // a cancellation report. This only repeats idempotent cleanup and
            // report storage, never the interpreter; cancellation is monotonic.
            terminal = cancelled_terminal;
            ctx.state_mut(|state| state.snapshot.terminal = Some(terminal.clone()));
            resolution = finalize(ctx, &start, &terminal).await?;
        }
        let cancelled = matches!(
            terminal,
            CodeExecutionTerminal::Interrupted {
                reason: CodeExecutionInterruption::HolderCancelled
                    | CodeExecutionInterruption::WorkflowCancelled,
                ..
            }
        );
        ctx.state_mut(|state| {
            state.snapshot.resolution = Some(resolution.clone());
            state.snapshot.phase = if cancelled {
                CodeExecutionPhase::Cancelled
            } else {
                CodeExecutionPhase::Resolved
            };
        });
        let envelope = harness::EmissionEnvelope::source_resolution(
            start.universe_id,
            start.execution_id.clone(),
            &start.holder_workflow_id,
            reply,
            resolution,
        );
        let _ = ctx
            .external_workflow(start.holder_workflow_id, None)
            .signal(
                AgentSessionWorkflow::deliver_emission,
                envelope,
                crate::workflows::signal_options(),
            )
            .await;
        if ctx.cancellation_token().is_cancelled() {
            Err(temporalio_sdk::WorkflowTermination::cancelled())
        } else {
            Ok(())
        }
    }

    #[signal(name = "deliver_emission")]
    pub fn deliver_emission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        envelope: harness::EmissionEnvelope,
    ) {
        if self
            .start
            .as_ref()
            .is_some_and(|start| is_holder_cancellation(start, &envelope))
        {
            self.holder_cancelled = true;
        }
    }

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> CodeExecutionSnapshot {
        self.snapshot.clone()
    }

    #[query(name = "workflow_tool_recovery")]
    pub fn workflow_tool_recovery(&self, _ctx: &WorkflowContextView) -> WorkflowToolRecoveryResult {
        recovery_result(&self.snapshot)
    }
}

async fn finalize(
    ctx: &WorkflowContext<CodeExecutionWorkflow>,
    start: &WorkflowToolStartArgs,
    terminal: &CodeExecutionTerminal,
) -> WorkflowResult<harness::PromiseResolution> {
    ctx.execute_activity(
        WorkflowActivities::code_finalize,
        CodeFinalizeActivityRequest {
            start: start.clone(),
            descriptor: ctx.state(|state| state.snapshot.descriptor.clone()),
            terminal: terminal.clone(),
        },
        finalization_options(),
    )
    .await
    .map_err(|error| {
        temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
            "code execution finalization failed: {error}"
        ))
        .into()
    })
}

/// Cancellation can replace a non-cancelled terminal once. Preserve any known
/// runner receipt so a late signal cannot discard already selected output.
fn terminal_after_cancellation(
    terminal: &CodeExecutionTerminal,
    holder_cancelled: bool,
    workflow_cancelled: bool,
) -> Option<CodeExecutionTerminal> {
    if (!holder_cancelled && !workflow_cancelled)
        || matches!(
            terminal,
            CodeExecutionTerminal::Interrupted {
                reason: CodeExecutionInterruption::HolderCancelled
                    | CodeExecutionInterruption::WorkflowCancelled,
                ..
            }
        )
    {
        return None;
    }
    let result = match terminal {
        CodeExecutionTerminal::Completed { result } => Some(result.clone()),
        CodeExecutionTerminal::Interrupted { result, .. } => result.clone(),
        CodeExecutionTerminal::Rejected { .. } => None,
    };
    Some(CodeExecutionTerminal::Interrupted {
        reason: if workflow_cancelled {
            CodeExecutionInterruption::WorkflowCancelled
        } else {
            CodeExecutionInterruption::HolderCancelled
        },
        result,
    })
}

async fn run_once(
    ctx: &WorkflowContext<CodeExecutionWorkflow>,
    descriptor: CodeExecutionDescriptor,
) -> CodeExecutionTerminal {
    let options = runner_options(descriptor.limits.timeout_ms);
    let mut activity = ctx.execute_activity(WorkflowActivities::code_run, descriptor, options);
    let outcome = {
        let holder_cancel = ctx.wait_for_state(|state| state.holder_cancelled);
        let external_cancel = ctx.cancelled().fuse();
        let work = (&mut activity).fuse();
        futures::pin_mut!(holder_cancel, external_cancel, work);
        select_biased! {
            _ = holder_cancel => Err(CodeExecutionInterruption::HolderCancelled),
            _ = external_cancel => Err(CodeExecutionInterruption::WorkflowCancelled),
            result = work => Ok(result),
        }
    };
    match outcome {
        Ok(Ok(result)) => CodeExecutionTerminal::Completed { result },
        Ok(Err(error)) => CodeExecutionTerminal::Interrupted {
            reason: if error.as_timeout().is_some() {
                CodeExecutionInterruption::ActivityTimedOut
            } else {
                CodeExecutionInterruption::ActivityFailed
            },
            result: None,
        },
        Err(reason) => {
            activity.cancel();
            let grace = ctx
                .timer_with_manual_cancellation(ACTIVITY_CANCELLATION_GRACE)
                .fuse();
            let work = activity.fuse();
            futures::pin_mut!(grace, work);
            let result = select_biased! { result = work => result.ok(), _ = grace => None };
            CodeExecutionTerminal::Interrupted { reason, result }
        }
    }
}

fn validate_start(
    workflow_id: &str,
    start: &WorkflowToolStartArgs,
) -> Result<harness::PromiseId, &'static str> {
    if workflow_id != start.execution_id
        || start.universe_id != start.invocation.session_universe_id
        || start.holder_workflow_id
            != crate::compose_workflow_id(start.universe_id, &start.invocation.session_id)
    {
        return Err("code execution workflow, holder, or universe identity mismatch");
    }
    let promises = start
        .invocation
        .completion_promises
        .as_ref()
        .ok_or("code execution requires one joined reply promise")?;
    if promises.len() != 1 {
        return Err("code execution requires exactly one joined reply promise");
    }
    promises
        .get(harness::REPLY_COMPLETION_KEY)
        .cloned()
        .ok_or("code execution is missing its joined reply promise")
}

fn descriptor_matches_start(
    descriptor: &CodeExecutionDescriptor,
    start: &WorkflowToolStartArgs,
) -> bool {
    descriptor.validate().is_ok()
        && descriptor.execution_id == start.execution_id
        && descriptor.session_workflow_id == start.holder_workflow_id
}

fn is_holder_cancellation(
    start: &WorkflowToolStartArgs,
    envelope: &harness::EmissionEnvelope,
) -> bool {
    let harness::EmissionProducer::Session {
        universe_id,
        session_id,
        ..
    } = &envelope.producer
    else {
        return false;
    };
    let harness::EmissionBody::InvocationCancellation {
        invocation_id,
        completion_key,
        promise_id,
    } = &envelope.body
    else {
        return false;
    };
    *universe_id == start.universe_id
        && *session_id == start.invocation.session_id
        && crate::compose_workflow_id(*universe_id, session_id) == start.holder_workflow_id
        && *invocation_id == start.invocation.invocation_id
        && completion_key == harness::REPLY_COMPLETION_KEY
        && start
            .invocation
            .completion_promises
            .as_ref()
            .and_then(|promises| promises.get(harness::REPLY_COMPLETION_KEY))
            == Some(promise_id)
}

fn recovery_result(snapshot: &CodeExecutionSnapshot) -> WorkflowToolRecoveryResult {
    WorkflowToolRecoveryResult {
        resolutions: snapshot
            .resolution
            .clone()
            .map(|resolution| [(harness::REPLY_COMPLETION_KEY.to_owned(), resolution)].into())
            .unwrap_or_default(),
    }
}

fn preparation_options() -> ActivityOptions {
    bounded_options(Duration::from_secs(60), Duration::from_secs(90), 3)
}

fn runner_options(timeout_ms: u64) -> ActivityOptions {
    let timeout = Duration::from_millis(timeout_ms);
    bounded_options(
        timeout.saturating_add(Duration::from_secs(30)),
        timeout.saturating_add(Duration::from_secs(60)),
        1,
    )
}

fn finalization_options() -> ActivityOptions {
    bounded_options(Duration::from_secs(30), Duration::from_secs(60), 3)
}

fn bounded_options(
    start_to_close: Duration,
    schedule_to_close: Duration,
    maximum_attempts: i32,
) -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::ScheduleAndStartToClose {
        start_to_close,
        schedule_to_close,
    })
    .heartbeat_timeout(ACTIVITY_HEARTBEAT)
    .cancellation_type(ActivityCancellationType::WaitCancellationCompleted)
    .cancellation_token(WorkflowCancellationToken::new())
    .retry_policy(RetryPolicy {
        initial_interval: Some(
            Duration::from_secs(1)
                .try_into()
                .expect("static duration fits"),
        ),
        maximum_interval: Some(
            Duration::from_secs(5)
                .try_into()
                .expect("static duration fits"),
        ),
        backoff_coefficient: 2.0,
        maximum_attempts,
        non_retryable_error_types: Vec::new(),
    })
    .build()
}

#[cfg(test)]
mod tests {
    use harness::{BlobRef, EmissionEnvelope, EventSeq, PromiseId, PromiseResolution};

    use super::*;

    fn start() -> WorkflowToolStartArgs {
        let universe_id = uuid::Uuid::from_u128(12);
        let session_id = harness::SessionId::new("code-parent");
        WorkflowToolStartArgs {
            universe_id,
            holder_workflow_id: crate::compose_workflow_id(universe_id, &session_id),
            execution_id: "wte:code-attempt".to_owned(),
            invocation: harness::WorkflowToolInvocation {
                invocation_id: harness::WorkflowToolInvocationId::new(format!(
                    "wti:sha256:{}",
                    "a".repeat(64)
                )),
                tool_id: harness::WorkflowToolId::new("lightspeed.code.execute.v1"),
                semantic_type: "lightspeed.code.execute.v1".to_owned(),
                schema_revision: 1,
                binding_fingerprint: "binding:code".to_owned(),
                session_universe_id: universe_id,
                session_id,
                run_id: harness::RunId::new(1),
                turn_id: harness::TurnId::new(1),
                tool_batch_id: harness::ToolBatchId::new(1),
                tool_call_id: harness::ToolCallId::new("call-code"),
                arguments_ref: BlobRef::from_bytes(b"{}"),
                execution_context_ref: Some(BlobRef::from_bytes(b"pinned-context")),
                completion_promises: Some(
                    [(
                        harness::REPLY_COMPLETION_KEY.to_owned(),
                        PromiseId::from_number(1),
                    )]
                    .into(),
                ),
            },
        }
    }

    fn cancellation(start: &WorkflowToolStartArgs) -> EmissionEnvelope {
        EmissionEnvelope::invocation_cancellation(
            start.universe_id,
            start.invocation.session_id.clone(),
            EventSeq::new(9),
            start.invocation.invocation_id.clone(),
            harness::REPLY_COMPLETION_KEY.to_owned(),
            PromiseId::from_number(1),
        )
    }

    #[test]
    fn start_requires_exact_workflow_universe_holder_and_one_reply() {
        let valid = start();
        assert_eq!(
            validate_start(&valid.execution_id, &valid),
            Ok(PromiseId::from_number(1))
        );
        assert!(validate_start("other-workflow", &valid).is_err());
        let mut invalid = valid.clone();
        invalid.universe_id = uuid::Uuid::from_u128(13);
        assert!(validate_start(&invalid.execution_id, &invalid).is_err());
        let mut invalid = valid.clone();
        invalid.holder_workflow_id =
            crate::compose_workflow_id(valid.universe_id, &harness::SessionId::new("other"));
        assert!(validate_start(&invalid.execution_id, &invalid).is_err());
        let mut invalid = valid.clone();
        invalid.invocation.completion_promises = None;
        assert!(validate_start(&invalid.execution_id, &invalid).is_err());
        let mut invalid = valid.clone();
        invalid
            .invocation
            .completion_promises
            .as_mut()
            .unwrap()
            .insert("another-key".to_owned(), PromiseId::from_number(2));
        assert!(validate_start(&invalid.execution_id, &invalid).is_err());
    }

    #[test]
    fn cancellation_requires_the_complete_pinned_holder_and_promise_identity() {
        let start = start();
        let valid = cancellation(&start);
        assert!(is_holder_cancellation(&start, &valid));
        let mut wrong = valid.clone();
        wrong.producer = harness::EmissionProducer::Workflow {
            universe_id: start.universe_id,
            workflow_id: start.holder_workflow_id.clone(),
        };
        assert!(!is_holder_cancellation(&start, &wrong));
        let mut wrong = valid.clone();
        if let harness::EmissionProducer::Session { universe_id, .. } = &mut wrong.producer {
            *universe_id = uuid::Uuid::from_u128(13);
        }
        assert!(!is_holder_cancellation(&start, &wrong));
        let mut wrong = valid.clone();
        if let harness::EmissionProducer::Session { session_id, .. } = &mut wrong.producer {
            *session_id = harness::SessionId::new("other-session");
        }
        assert!(!is_holder_cancellation(&start, &wrong));
        for field in ["invocation", "key", "promise"] {
            let mut wrong = valid.clone();
            let harness::EmissionBody::InvocationCancellation {
                invocation_id,
                completion_key,
                promise_id,
            } = &mut wrong.body
            else {
                panic!("cancellation")
            };
            match field {
                "invocation" => {
                    *invocation_id = harness::WorkflowToolInvocationId::new(format!(
                        "wti:sha256:{}",
                        "b".repeat(64)
                    ))
                }
                "key" => *completion_key = "other".to_owned(),
                "promise" => *promise_id = PromiseId::from_number(2),
                _ => unreachable!(),
            }
            assert!(!is_holder_cancellation(&start, &wrong), "reject {field}");
        }
    }

    #[test]
    fn recovery_exposes_only_a_committed_resolution() {
        let mut snapshot = CodeExecutionSnapshot::default();
        assert!(recovery_result(&snapshot).resolutions.is_empty());
        snapshot.phase = CodeExecutionPhase::Finalizing;
        snapshot.terminal = Some(CodeExecutionTerminal::Completed {
            result: crate::CodeRunActivityResult {
                report_ref: BlobRef::from_bytes(b"full report"),
                succeeded: false,
            },
        });
        assert!(recovery_result(&snapshot).resolutions.is_empty());
        let resolution = PromiseResolution::Resolved {
            payload_ref: Some(BlobRef::from_bytes(b"compact output")),
        };
        snapshot.resolution = Some(resolution.clone());
        assert_eq!(
            recovery_result(&snapshot).resolutions,
            [(harness::REPLY_COMPLETION_KEY.to_owned(), resolution)].into()
        );
        let encoded = serde_json::to_vec(&snapshot).expect("snapshot serializes");
        let restored = serde_json::from_slice(&encoded).expect("snapshot deserializes");
        assert_eq!(recovery_result(&snapshot), recovery_result(&restored));
        assert!(
            encoded.len() < 1024,
            "durable state contains references only"
        );
    }

    #[test]
    fn cancellation_during_finalization_preserves_the_runner_receipt_once() {
        let receipt = crate::CodeRunActivityResult {
            report_ref: BlobRef::from_bytes(b"selected output and outcomes"),
            succeeded: false,
        };
        let completed = CodeExecutionTerminal::Completed {
            result: receipt.clone(),
        };
        assert_eq!(terminal_after_cancellation(&completed, false, false), None);
        let cancelled = terminal_after_cancellation(&completed, true, false)
            .expect("late holder cancellation changes the final report");
        assert_eq!(
            cancelled,
            CodeExecutionTerminal::Interrupted {
                reason: CodeExecutionInterruption::HolderCancelled,
                result: Some(receipt.clone()),
            }
        );
        // An external cancellation arriving during the second finalization
        // cannot cause unbounded cleanup/report activity scheduling.
        assert_eq!(terminal_after_cancellation(&cancelled, true, true), None);
        assert_eq!(
            terminal_after_cancellation(&completed, false, true),
            Some(CodeExecutionTerminal::Interrupted {
                reason: CodeExecutionInterruption::WorkflowCancelled,
                result: Some(receipt),
            })
        );
    }

    #[test]
    fn late_cancellation_also_covers_missing_receipts_and_preparation_failures() {
        for terminal in [
            CodeExecutionTerminal::Interrupted {
                reason: CodeExecutionInterruption::ActivityTimedOut,
                result: None,
            },
            CodeExecutionTerminal::Rejected {
                error_ref: BlobRef::from_bytes(b"preparation failed"),
            },
        ] {
            assert_eq!(
                terminal_after_cancellation(&terminal, true, false),
                Some(CodeExecutionTerminal::Interrupted {
                    reason: CodeExecutionInterruption::HolderCancelled,
                    result: None,
                })
            );
        }
    }

    #[test]
    fn runner_requires_a_valid_descriptor_for_the_exact_admitted_scope() {
        let start = start();
        let fixture =
            crate::workflow_contract::export().manifest["vectors"]["codeExecution"]["descriptor"]
                .clone();
        let mut descriptor: CodeExecutionDescriptor =
            serde_json::from_value(fixture).expect("valid descriptor fixture");
        descriptor.execution_id = start.execution_id.clone();
        descriptor.session_workflow_id = start.holder_workflow_id.clone();
        assert!(descriptor_matches_start(&descriptor, &start));
        let mut invalid = descriptor.clone();
        invalid.execution_id = "other-scope".to_owned();
        assert!(!descriptor_matches_start(&invalid, &start));
        let mut invalid = descriptor.clone();
        invalid.session_workflow_id =
            crate::compose_workflow_id(start.universe_id, &harness::SessionId::new("other-parent"));
        assert!(!descriptor_matches_start(&invalid, &start));
        let mut invalid = descriptor;
        invalid.limits.timeout_ms = 0;
        assert!(!descriptor_matches_start(&invalid, &start));
    }

    #[test]
    fn runner_never_retries_and_all_stages_have_bounded_detached_cleanup() {
        let runner = runner_options(60_000);
        assert_eq!(
            runner.retry_policy.as_ref().unwrap().raw().maximum_attempts,
            1
        );
        assert!(
            matches!(runner.close_timeouts, ActivityCloseTimeouts::ScheduleAndStartToClose { start_to_close, schedule_to_close } if start_to_close == Duration::from_secs(90) && schedule_to_close == Duration::from_secs(120))
        );
        for options in [preparation_options(), runner, finalization_options()] {
            assert!(
                options.cancellation_token.is_some(),
                "workflow cancellation must not short-circuit cleanup"
            );
            assert_eq!(
                options.cancellation_type,
                ActivityCancellationType::WaitCancellationCompleted
            );
            assert_eq!(options.heartbeat_timeout, Some(ACTIVITY_HEARTBEAT));
            assert!(matches!(
                options.close_timeouts,
                ActivityCloseTimeouts::ScheduleAndStartToClose { .. }
            ));
            assert!(options.retry_policy.unwrap().raw().maximum_attempts > 0);
            assert!(
                options.task_queue.is_none(),
                "activities use this workflow's code queue"
            );
        }
    }
}
