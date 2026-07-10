use super::*;
use engine::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentJoins, EventSeq,
    PromiseScope, PromiseSource, PromiseStatus, RunId, RunRecord, RunStatus,
    RunTerminalNotifyIntent, ToolBatchId, ToolCallId, TurnId,
};

#[test]
fn pending_admissions_are_fifo() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.queue_admission(admission(deliver_message("submit_1")));
    workflow.queue_admission(admission(deliver_message("submit_2")));

    let pending = std::mem::take(&mut workflow.pending_admissions);
    assert_eq!(
        pending[0].command.submission_id_for_test(),
        Some(SubmissionId::new("submit_1"))
    );
    assert_eq!(
        pending[1].command.submission_id_for_test(),
        Some(SubmissionId::new("submit_2"))
    );
}

#[test]
fn admission_failure_status_does_not_poison_later_admission() {
    let mut workflow = AgentSessionWorkflow::default();
    let rejection = engine::CommandRejection::context_revision_conflict(3, 4);
    workflow.admission_failures.push(AgentAdmissionFailure {
        submission_id: Some(SubmissionId::new("submit_rejected")),
        correlation_token: Some("admit_test".to_owned()),
        kind: AgentAdmissionFailureKind::RejectedCommand,
        message: rejection.to_string(),
        rejection: Some(rejection.clone()),
    });
    workflow.queue_admission(admission(deliver_message("submit_later")));

    let status = workflow.status_snapshot();

    assert_eq!(status.pending_admissions, 1);
    assert_eq!(status.admission_failures.len(), 1);
    assert_eq!(
        status.admission_failures[0].submission_id.as_ref(),
        Some(&SubmissionId::new("submit_rejected"))
    );
    assert_eq!(
        status.admission_failures[0].kind,
        AgentAdmissionFailureKind::RejectedCommand
    );
    assert_eq!(
        status.admission_failures[0].correlation_token.as_deref(),
        Some("admit_test")
    );
    assert_eq!(status.admission_failures[0].rejection, Some(rejection));
    assert_eq!(status.last_error, None);
}

#[test]
fn submit_message_submission_id_is_available_for_failure_correlation() {
    let submission_id = SubmissionId::new("submit_test");
    let command = CoreAgentCommand::SubmitMessage(engine::SubmitMessageCommand {
        submission_id: Some(submission_id.clone()),
        input: user_input(engine::BlobRef::from_bytes(b"hello")),
    });

    assert_eq!(drive::command_submission_id(&command), Some(submission_id));
    assert_eq!(
        drive::command_submission_id(&CoreAgentCommand::CloseSession { force: false }),
        None
    );
}

#[test]
fn request_run_with_audio_input_needs_preprocessing() {
    let command = CoreAgentCommand::RequestRun(engine::RunRequestCommand {
        notify_on_terminal: Vec::new(),
        submission_id: Some(SubmissionId::new("submit_audio")),
        source: engine::RunRequestSource::Input {
            input: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                content_ref: engine::BlobRef::from_bytes(b"audio"),
                media_type: Some("audio/ogg".to_owned()),
                preview: Some("[audio]".to_owned()),
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            }],
        },
        run_config: crate::default_run_config(),
    });

    assert!(admissions::command_needs_input_preprocessing(&command));
}

#[test]
fn preprocess_failures_preserve_submission_id_for_admission_failure() {
    let failure = admissions::preprocess_failure_to_admission_failure(
        Some(SubmissionId::new("submit_audio")),
        PreprocessRunInputFailure {
            kind: PreprocessRunInputFailureKind::TranscriptionFailure,
            message: "missing OpenAI key".to_owned(),
        },
    );

    assert_eq!(
        failure.submission_id.as_ref(),
        Some(&SubmissionId::new("submit_audio"))
    );
    assert_eq!(
        failure.kind,
        AgentAdmissionFailureKind::TranscriptionFailure
    );
}

#[test]
fn environment_job_resolution_signal_queues_direct_promise_resolution() {
    let mut workflow = AgentSessionWorkflow::default();
    let payload_ref = engine::BlobRef::from_bytes(b"job output");
    workflow.queue_promise_source_resolution(PromiseSourceResolutionSignal {
        promise_id: "p1".to_owned(),
        result: engine::PromiseSourceCheckResult::Resolved {
            payload_ref: Some(payload_ref.clone()),
        },
    });

    assert!(matches!(
        &workflow.pending_admissions[0].command,
        CoreAgentCommand::ResolvePromise {
            promise_id,
            resolution: engine::PromiseResolution::Resolved {
                payload_ref: Some(actual),
            },
        } if promise_id.as_str() == "p1" && actual == &payload_ref
    ));
}

#[test]
fn close_on_terminal_requires_idle_open_session_with_completed_run() {
    let args = agent_session_args_with_close_on_terminal(true);
    let mut state = CoreAgentState::new();
    assert!(!drive::should_close_on_terminal(&args, &state));

    state.lifecycle.status = CoreAgentStatus::Open;
    assert!(!drive::should_close_on_terminal(&args, &state));

    state.runs.completed.push(RunRecord {
        notify_on_terminal: Vec::new(),
        run_id: RunId::new(1),
        status: RunStatus::Completed,
        submission_id: None,
        origin: engine::RunOrigin::Requested,
        submission_digest: None,
        output_ref: None,
        failure: None,
    });
    assert!(drive::should_close_on_terminal(&args, &state));
    assert!(!drive::should_close_on_terminal(
        &agent_session_args_with_close_on_terminal(false),
        &state
    ));

    state.promises.promises.insert(
        engine::PromiseId::new("p_detached"),
        promise("p_detached", PromiseStatus::Pending),
    );
    assert!(!drive::should_close_on_terminal(&args, &state));
    state
        .promises
        .promises
        .get_mut(&engine::PromiseId::new("p_detached"))
        .expect("promise")
        .status = PromiseStatus::Resolved;
    assert!(drive::should_close_on_terminal(&args, &state));

    state.lifecycle.status = CoreAgentStatus::Closed;
    assert!(!drive::should_close_on_terminal(&args, &state));
}

#[test]
fn continue_as_new_policy_uses_server_suggestion() {
    assert!(wait_loop::should_continue_as_new(true, 1, Some(10)));
}

#[test]
fn continue_as_new_policy_uses_history_threshold() {
    assert!(wait_loop::should_continue_as_new(false, 10, Some(10)));
    assert!(!wait_loop::should_continue_as_new(false, 9, Some(10)));
}

#[test]
fn continue_as_new_policy_uses_default_threshold() {
    assert!(wait_loop::should_continue_as_new(
        false,
        DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
        None
    ));
    assert!(!wait_loop::should_continue_as_new(
        false,
        DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD - 1,
        None
    ));
}

fn deliver_message(submission_id: &str) -> CoreAgentCommand {
    CoreAgentCommand::SubmitMessage(engine::SubmitMessageCommand {
        submission_id: Some(SubmissionId::new(submission_id)),
        input: user_input(engine::BlobRef::from_bytes(submission_id.as_bytes())),
    })
}

fn request_run_with_notify(submission_id: &str) -> CoreAgentCommand {
    CoreAgentCommand::RequestRun(engine::RunRequestCommand {
        notify_on_terminal: vec![RunTerminalNotifyIntent {
            holder_workflow_id: "universe/holder".to_owned(),
            token: "promise_request".to_owned(),
        }],
        submission_id: Some(SubmissionId::new(submission_id)),
        source: engine::RunRequestSource::Input {
            input: user_input(engine::BlobRef::from_bytes(submission_id.as_bytes())),
        },
        run_config: crate::default_run_config(),
    })
}

fn user_input(content_ref: engine::BlobRef) -> Vec<ContextEntryInput> {
    vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: None,
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }]
}

fn admission(command: CoreAgentCommand) -> AgentAdmission {
    AgentAdmission {
        command,
        correlation_token: None,
    }
}

fn agent_session_args_with_close_on_terminal(close_on_terminal: bool) -> AgentSessionArgs {
    AgentSessionArgs {
        universe_id: uuid::Uuid::nil(),
        session_id: SessionId::new("session_test"),
        display_name: None,
        session_config: crate::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "gpt-test".to_owned(),
        }),
        max_steps_per_input: None,
        continue_as_new_history_threshold: None,
        close_on_terminal,
    }
}

fn pending_resume(batch_id: u64) -> PendingToolBatchResume {
    PendingToolBatchResume {
        batch_id: ToolBatchId::new(batch_id),
        command: engine::ResumeAwaitCommand {
            run_id: RunId::new(1),
            batch_id: ToolBatchId::new(batch_id),
            claim: engine::WakeReason::Timeout,
            claim_observed_at_ms: 1_000,
            output: engine::AwaitOutputRefs {
                output_ref: engine::BlobRef::from_bytes(b"await output"),
                summary_ref: engine::BlobRef::from_bytes(b"await summary"),
            },
        },
    }
}

fn pending_promise_cancellation(promise_id: &str) -> PendingPromiseCancellation {
    PendingPromiseCancellation {
        promise_id: promise_id.to_owned(),
        source: PromiseSource::Timer { fire_at_ms: 1_000 },
    }
}

fn workflow_with_parked_mailbox_await() -> AgentSessionWorkflow {
    workflow_with_parked_await(engine::AwaitSpec {
        promise_ids: Vec::new(),
        mode: engine::AwaitMode::All,
        mailbox: true,
        deadline_at_ms: None,
    })
}

fn workflow_with_parked_await(spec: engine::AwaitSpec) -> AgentSessionWorkflow {
    let mut workflow = AgentSessionWorkflow::default();
    let run_id = RunId::new(1);
    let turn_id = TurnId::new(1);
    let batch_id = ToolBatchId::new(1);
    let call_id = ToolCallId::new("call_await");
    let mut tool_batches = std::collections::BTreeMap::new();
    tool_batches.insert(
        batch_id,
        engine::ActiveToolBatch {
            batch_id,
            run_id,
            turn_id,
            calls: vec![engine::ToolCallState {
                call: engine::ObservedToolCall {
                    call_id: call_id.clone(),
                    tool_name: engine::ToolName::new("await"),
                    provider_kind: None,
                    arguments_ref: engine::BlobRef::from_bytes(b"{}"),
                    native_call_ref: None,
                },
                status: engine::ToolCallStatus::Pending,
                execution_policy: None,
                execution_target: None,
                result: None,
            }],
        },
    );
    workflow.core_state.runs.active = Some(engine::ActiveRun {
        run_id,
        status: RunStatus::Parked,
        submission_id: None,
        origin: engine::RunOrigin::Requested,
        source: engine::RunSource::Input {
            input: user_input(engine::BlobRef::from_bytes(b"start")),
        },
        input_entry_ids: Vec::new(),
        input_consumed_by_turn_id: None,
        run_config: crate::default_run_config(),
        config_revision: 0,
        steering: Vec::new(),
        turns: std::collections::BTreeMap::new(),
        active_turn_id: None,
        active_tool_batch_id: Some(batch_id),
        parked_await: Some(engine::ParkedAwait {
            batch_id,
            call_id,
            spec,
        }),
        cancellation_grace_turn_id: None,
        tool_batches,
        completed_tool_batches: std::collections::BTreeMap::new(),
        output_ref: None,
        failure: None,
        notify_on_terminal: Vec::new(),
    });
    workflow
}

trait CommandSubmissionIdForTest {
    fn submission_id_for_test(&self) -> Option<SubmissionId>;
}

impl CommandSubmissionIdForTest for CoreAgentCommand {
    fn submission_id_for_test(&self) -> Option<SubmissionId> {
        drive::command_submission_id(self)
    }
}

#[test]
fn cancelling_watchdog_arms_holds_rearms_and_disarms() {
    let now = 1_000;
    let armed = watchdog::next_cancelling_watchdog(Some(7), None, now);
    assert_eq!(
        armed,
        Some(CancellingWatchdog {
            run_id: 7,
            since_ms: now
        })
    );
    // Holds its original deadline while the same run keeps cancelling.
    let held = watchdog::next_cancelling_watchdog(Some(7), armed, now + 500);
    assert_eq!(held, armed);
    // A different cancelling run restarts the clock.
    let rearmed = watchdog::next_cancelling_watchdog(Some(8), held, now + 900);
    assert_eq!(
        rearmed,
        Some(CancellingWatchdog {
            run_id: 8,
            since_ms: now + 900
        })
    );
    // Disarms once no run is cancelling.
    assert_eq!(
        watchdog::next_cancelling_watchdog(None, rearmed, now + 950),
        None
    );
}

#[test]
fn cancelling_watchdog_wake_is_since_plus_timeout() {
    let mut workflow = AgentSessionWorkflow::default();
    assert_eq!(watchdog::cancelling_watchdog_wake_ms(&workflow), None);
    workflow.cancelling_watchdog = Some(CancellingWatchdog {
        run_id: 1,
        since_ms: 2_000,
    });
    assert_eq!(
        watchdog::cancelling_watchdog_wake_ms(&workflow),
        Some(2_000 + watchdog::CANCELLING_WATCHDOG_MS)
    );
}

#[test]
fn promise_resolution_signal_queues_resolve_promise_admission() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.queue_promise_resolution(PromiseResolutionSignal {
        token: "promise_a".to_owned(),
        status: RunStatus::Completed,
        output_ref: Some(engine::BlobRef::from_bytes(b"result")),
        failure_message_ref: None,
    });
    workflow.queue_promise_resolution(PromiseResolutionSignal {
        token: "promise_b".to_owned(),
        status: RunStatus::Cancelled,
        output_ref: None,
        failure_message_ref: None,
    });

    assert_eq!(workflow.pending_admissions.len(), 2);
    match &workflow.pending_admissions[0].command {
        CoreAgentCommand::ResolvePromise {
            promise_id,
            resolution: engine::PromiseResolution::Resolved { payload_ref },
        } => {
            assert_eq!(promise_id.as_str(), "promise_a");
            assert!(payload_ref.is_some());
        }
        other => panic!("expected resolved promise admission, got {other:?}"),
    }
    match &workflow.pending_admissions[1].command {
        CoreAgentCommand::ResolvePromise {
            promise_id,
            resolution: engine::PromiseResolution::Failed { .. },
        } => assert_eq!(promise_id.as_str(), "promise_b"),
        other => panic!("expected failed promise admission, got {other:?}"),
    }
}

#[test]
fn terminal_run_with_notify_intent_queues_promise_notification() {
    let mut workflow = AgentSessionWorkflow::default();
    let output_ref = engine::BlobRef::from_bytes(b"done");
    workflow.core_state.runs.completed.push(RunRecord {
        run_id: RunId::new(3),
        status: RunStatus::Completed,
        submission_id: None,
        origin: engine::RunOrigin::Requested,
        submission_digest: None,
        output_ref: Some(output_ref.clone()),
        failure: None,
        notify_on_terminal: vec![RunTerminalNotifyIntent {
            holder_workflow_id: "universe/parent_session".to_owned(),
            token: "promise_parent".to_owned(),
        }],
    });
    let entry = engine::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(1),
        },
        observed_at_ms: 100,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::Run(RunEvent::Completed {
            run_id: RunId::new(3),
            output_ref: Some(output_ref.clone()),
        }),
    };

    workflow.queue_promise_notifications_for_entries(std::slice::from_ref(&entry));

    assert_eq!(workflow.pending_promise_notifications.len(), 1);
    let pending = &workflow.pending_promise_notifications[0];
    assert_eq!(pending.holder_workflow_id, "universe/parent_session");
    assert_eq!(pending.signal.token, "promise_parent");
    assert_eq!(pending.signal.status, RunStatus::Completed);
    assert_eq!(pending.signal.output_ref.as_ref(), Some(&output_ref));
    // A run without intents queues nothing.
    workflow.pending_promise_notifications.clear();
    workflow.core_state.runs.completed[0]
        .notify_on_terminal
        .clear();
    workflow.queue_promise_notifications_for_entries(std::slice::from_ref(&entry));
    assert!(workflow.pending_promise_notifications.is_empty());
}

fn promise(id: &str, status: PromiseStatus) -> engine::Promise {
    promise_with_source(
        id,
        status,
        PromiseSource::Run {
            target_session_id: "child".to_owned(),
            target_run_id: 1,
        },
        PromiseScope::Session,
    )
}

fn promise_with_source(
    id: &str,
    status: PromiseStatus,
    source: PromiseSource,
    scope: PromiseScope,
) -> engine::Promise {
    engine::Promise {
        promise_id: engine::PromiseId::new(id),
        source,
        scope,
        status,
        payload_ref: None,
        error_ref: None,
        deadline_ms: None,
    }
}

fn add_promises(workflow: &mut AgentSessionWorkflow, promises: Vec<engine::Promise>) {
    for promise in promises {
        workflow
            .core_state
            .promises
            .promises
            .insert(promise.promise_id.clone(), promise);
    }
}

fn await_spec(
    ids: &[&str],
    mode: engine::AwaitMode,
    deadline_at_ms: Option<u64>,
) -> engine::AwaitSpec {
    engine::AwaitSpec {
        promise_ids: ids.iter().map(|id| engine::PromiseId::new(*id)).collect(),
        mode,
        mailbox: false,
        deadline_at_ms,
    }
}

#[test]
fn workflow_await_waits_for_every_promise_in_all_mode() {
    let mut workflow =
        workflow_with_parked_await(await_spec(&["p1", "p2"], engine::AwaitMode::All, None));
    add_promises(
        &mut workflow,
        vec![
            promise("p1", PromiseStatus::Resolved),
            promise("p2", PromiseStatus::Pending),
        ],
    );
    assert!(!awaits::has_satisfied_await(&workflow));

    add_promises(
        &mut workflow,
        vec![
            promise("p1", PromiseStatus::Resolved),
            promise("p2", PromiseStatus::Failed),
        ],
    );
    assert!(awaits::has_satisfied_await(&workflow));
}

#[test]
fn workflow_await_resolves_any_mode_on_first_terminal_promise() {
    let mut workflow =
        workflow_with_parked_await(await_spec(&["p1", "p2"], engine::AwaitMode::Any, None));
    add_promises(
        &mut workflow,
        vec![
            promise("p1", PromiseStatus::Cancelled),
            promise("p2", PromiseStatus::Pending),
        ],
    );
    assert!(awaits::has_satisfied_await(&workflow));
}

#[test]
fn workflow_await_deadline_uses_timer_not_state_condition() {
    let mut workflow =
        workflow_with_parked_await(await_spec(&["p1"], engine::AwaitMode::All, Some(1_000)));
    add_promises(&mut workflow, vec![promise("p1", PromiseStatus::Pending)]);
    assert!(!awaits::has_satisfied_await(&workflow));
    assert_eq!(awaits::nearest_await_wake_ms(&workflow), Some(1_000));
}

#[test]
fn promise_snapshot_reports_pending_promise() {
    let spec = await_spec(&["p1"], engine::AwaitMode::All, Some(1_000));
    let mut workflow = workflow_with_parked_await(spec.clone());
    add_promises(&mut workflow, vec![promise("p1", PromiseStatus::Pending)]);
    let snapshot = awaits::promise_snapshot(&spec, &workflow.core_state);
    assert_eq!(snapshot[0].status, "pending");
}

#[test]
fn mailbox_await_wakes_on_engine_buffered_message() {
    let mut workflow = workflow_with_parked_mailbox_await();
    assert!(!awaits::has_satisfied_await(&workflow));

    workflow.queue_admission(admission(deliver_message("submit_regular")));
    assert!(!awaits::has_satisfied_await(&workflow));
    workflow.pending_admissions.clear();

    workflow.queue_admission(admission(request_run_with_notify("fleet_request_1")));
    assert!(!awaits::has_satisfied_await(&workflow));
    workflow.pending_admissions.clear();

    let input = user_input(engine::BlobRef::from_bytes(b"fleet_send_1"));
    workflow
        .core_state
        .runs
        .messages
        .push(engine::BufferedMessage {
            message_id: engine::MessageId::new(1),
            submission_id: Some(SubmissionId::new("fleet_send_1")),
            submission_digest: engine::message_submission_digest(&input),
            input,
            run_config: crate::default_run_config(),
            config_revision: 0,
            status: engine::MessageStatus::Buffered,
            consumed_by_run_id: None,
            promoted_to_run_id: None,
        });
    assert!(awaits::has_satisfied_await(&workflow));
}

#[test]
fn continue_as_new_allows_pending_sources_and_parked_awaits() {
    let mut workflow = workflow_with_parked_await(engine::AwaitSpec {
        promise_ids: vec![
            engine::PromiseId::new("p_child"),
            engine::PromiseId::new("p_request"),
            engine::PromiseId::new("p_env"),
            engine::PromiseId::new("p_timer"),
            engine::PromiseId::new("p_detached"),
        ],
        mode: engine::AwaitMode::All,
        mailbox: true,
        deadline_at_ms: Some(50_000),
    });
    let promises = [
        promise_with_source(
            "p_child",
            PromiseStatus::Pending,
            PromiseSource::Run {
                target_session_id: "child".to_owned(),
                target_run_id: 10,
            },
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "p_request",
            PromiseStatus::Pending,
            PromiseSource::Run {
                target_session_id: "peer".to_owned(),
                target_run_id: 11,
            },
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "p_env",
            PromiseStatus::Pending,
            PromiseSource::EnvJob {
                instance_id: "evi_1".to_owned(),
                job_id: "job_1".to_owned(),
            },
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "p_timer",
            PromiseStatus::Pending,
            PromiseSource::Timer { fire_at_ms: 60_000 },
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "p_detached",
            PromiseStatus::Pending,
            PromiseSource::Run {
                target_session_id: "detached_child".to_owned(),
                target_run_id: 12,
            },
            PromiseScope::Session,
        ),
    ];
    for promise in promises {
        workflow
            .core_state
            .promises
            .promises
            .insert(promise.promise_id.clone(), promise);
    }

    let parked = awaits::parked_await(&workflow.core_state).expect("parked await");
    assert_eq!(parked.spec.promise_ids.len(), 5);
    assert_eq!(awaits::nearest_await_wake_ms(&workflow), Some(50_000));
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));
}

#[test]
fn promise_source_polls_rehydrate_from_pending_poll_sources() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.promise_source_polls.insert(
        "stale".to_owned(),
        PromiseSourcePoll {
            promise_id: "stale".to_owned(),
            source: PromiseSource::Timer { fire_at_ms: 1 },
            next_check_at_ms: 1,
            poll_attempt: 9,
        },
    );
    let env_source = PromiseSource::EnvJob {
        instance_id: "evi_1".to_owned(),
        job_id: "job_1".to_owned(),
    };
    let timer_source = PromiseSource::Timer { fire_at_ms: 60_000 };
    let promises = [
        promise_with_source(
            "p_env",
            PromiseStatus::Pending,
            env_source.clone(),
            PromiseScope::Session,
        ),
        promise_with_source(
            "p_timer",
            PromiseStatus::Pending,
            timer_source.clone(),
            PromiseScope::Session,
        ),
        promise_with_source(
            "p_child",
            PromiseStatus::Pending,
            PromiseSource::Run {
                target_session_id: "child".to_owned(),
                target_run_id: 10,
            },
            PromiseScope::Session,
        ),
        promise_with_source(
            "p_request",
            PromiseStatus::Pending,
            PromiseSource::Run {
                target_session_id: "peer".to_owned(),
                target_run_id: 11,
            },
            PromiseScope::Session,
        ),
        promise_with_source(
            "p_resolved_env",
            PromiseStatus::Resolved,
            PromiseSource::EnvJob {
                instance_id: "evi_1".to_owned(),
                job_id: "job_done".to_owned(),
            },
            PromiseScope::Session,
        ),
    ];
    for promise in promises {
        workflow
            .core_state
            .promises
            .promises
            .insert(promise.promise_id.clone(), promise);
    }

    promise_sources::reconcile_polls_for_state(&mut workflow, 10_000);

    assert_eq!(workflow.promise_source_polls.len(), 1);
    assert!(!workflow.promise_source_polls.contains_key("stale"));
    assert!(!workflow.promise_source_polls.contains_key("p_env"));
    assert!(!workflow.promise_source_polls.contains_key("p_child"));
    assert!(!workflow.promise_source_polls.contains_key("p_request"));
    assert!(!workflow.promise_source_polls.contains_key("p_resolved_env"));
    let timer_poll = workflow
        .promise_source_polls
        .get("p_timer")
        .expect("timer poll");
    assert_eq!(timer_poll.source, timer_source);
    assert_eq!(timer_poll.next_check_at_ms, 60_000);
    assert_eq!(timer_poll.poll_attempt, 0);
    assert_eq!(promise_sources::nearest_wake_ms(&workflow), Some(60_000));
    assert!(promise_sources::has_unconfirmed_subscriptions(&workflow));
    workflow
        .confirmed_promise_source_subscriptions
        .insert("p_env".to_owned());
    assert!(!promise_sources::has_unconfirmed_subscriptions(&workflow));
}

#[test]
fn continue_as_new_is_blocked_by_transport_only() {
    let mut workflow = AgentSessionWorkflow::default();
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));

    workflow.queue_admission(admission(deliver_message("submit_1")));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_admissions.clear();

    workflow.pending_tool_batch_resumes.push(pending_resume(1));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_tool_batch_resumes.clear();

    workflow
        .pending_promise_notifications
        .push(PendingPromiseNotification {
            holder_workflow_id: "universe/parent".to_owned(),
            signal: PromiseResolutionSignal {
                token: "promise_1".to_owned(),
                status: RunStatus::Completed,
                output_ref: None,
                failure_message_ref: None,
            },
        });
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_promise_notifications.clear();

    workflow
        .pending_promise_cancellations
        .push(pending_promise_cancellation("p1"));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_promise_cancellations.clear();

    // Log-derived state (pending promises) never blocks continue-as-new.
    workflow.core_state.promises.promises.insert(
        engine::PromiseId::new("p1"),
        promise("p1", PromiseStatus::Pending),
    );
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));
}

#[test]
fn closed_quiescent_workflow_can_complete() {
    let mut workflow = AgentSessionWorkflow::default();
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));

    workflow.initialized = true;
    workflow.core_state.lifecycle.status = CoreAgentStatus::Closed;
    assert!(wait_loop::workflow_state_is_closed_and_quiescent(&workflow));

    workflow.pending_tool_batch_resumes.push(pending_resume(1));
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.pending_tool_batch_resumes.clear();

    workflow
        .pending_promise_notifications
        .push(PendingPromiseNotification {
            holder_workflow_id: "universe/parent".to_owned(),
            signal: PromiseResolutionSignal {
                token: "promise_1".to_owned(),
                status: RunStatus::Completed,
                output_ref: None,
                failure_message_ref: None,
            },
        });
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.pending_promise_notifications.clear();

    workflow
        .pending_promise_cancellations
        .push(pending_promise_cancellation("p1"));
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.pending_promise_cancellations.clear();

    assert!(wait_loop::workflow_state_is_closed_and_quiescent(&workflow));
}
