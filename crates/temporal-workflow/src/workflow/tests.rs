use super::*;
use engine::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole, RunId, RunRecord, RunStatus,
    ToolBatchId, ToolInvocationBatchResult, TurnId,
};

#[test]
fn pending_admissions_are_fifo() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.queue_admission(admission(request_run("submit_1")));
    workflow.queue_admission(admission(request_run("submit_2")));

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
    workflow.admission_failures.push(AgentAdmissionFailure {
        submission_id: Some(SubmissionId::new("submit_rejected")),
        context_key: None,
        kind: AgentAdmissionFailureKind::RejectedCommand,
        message: "session must be open".to_owned(),
    });
    workflow.queue_admission(admission(request_run("submit_later")));

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
    assert_eq!(status.last_error, None);
}

#[test]
fn request_run_submission_id_is_available_for_failure_correlation() {
    let submission_id = SubmissionId::new("submit_test");
    let command = CoreAgentCommand::RequestRun(engine::RunRequestCommand {
        submission_id: Some(submission_id.clone()),
        source: engine::RunRequestSource::Input {
            input: user_input(engine::BlobRef::from_bytes(b"hello")),
        },
        run_config: crate::default_run_config(),
    });

    assert_eq!(drive::command_submission_id(&command), Some(submission_id));
    assert_eq!(
        drive::command_submission_id(&CoreAgentCommand::CloseSession),
        None
    );
}

#[test]
fn request_run_with_audio_input_needs_preprocessing() {
    let command = CoreAgentCommand::RequestRun(engine::RunRequestCommand {
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
fn subscribe_run_against_completed_run_queues_notification_without_storing_subscription() {
    let mut workflow = AgentSessionWorkflow::default();
    let output_ref = engine::BlobRef::from_bytes(b"done");
    workflow.core_state.runs.completed.push(RunRecord {
        run_id: RunId::new(1),
        status: RunStatus::Completed,
        submission_id: None,
        submission_digest: None,
        output_ref: Some(output_ref.clone()),
        failure: None,
    });

    let subscription = run_subscription("sub_1", "token_1", 1);
    workflow.subscribe_to_run(subscription.clone());

    assert!(workflow.run_subscriptions.is_empty());
    assert_eq!(workflow.pending_terminal_notifications.len(), 1);
    let pending = &workflow.pending_terminal_notifications[0];
    assert_eq!(pending.subscription, subscription);
    assert_eq!(pending.notification.correlation_token, "token_1");
    assert_eq!(pending.notification.run_id, RunId::new(1));
    assert_eq!(pending.notification.status, RunStatus::Completed);
    assert_eq!(pending.notification.output_ref.as_ref(), Some(&output_ref));
}

#[test]
fn terminal_event_fanout_removes_matching_subscriptions_once() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.subscribe_to_run(run_subscription("sub_1", "token_1", 1));
    workflow.subscribe_to_run(run_subscription("sub_2", "token_2", 1));
    workflow.subscribe_to_run(run_subscription("sub_3", "token_3", 2));

    workflow.queue_terminal_notifications(terminal_notification("", 1, RunStatus::Completed));

    assert_eq!(workflow.run_subscriptions.len(), 1);
    assert!(workflow.run_subscriptions.contains_key("sub_3"));
    assert_eq!(workflow.pending_terminal_notifications.len(), 2);
    let tokens = workflow
        .pending_terminal_notifications
        .iter()
        .map(|pending| pending.notification.correlation_token.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tokens, vec!["token_1", "token_2"]);

    workflow.queue_terminal_notifications(terminal_notification("", 1, RunStatus::Completed));

    assert_eq!(workflow.pending_terminal_notifications.len(), 2);
    assert_eq!(workflow.run_subscriptions.len(), 1);
}

#[test]
fn unsubscribe_run_removes_stored_and_pending_notifications() {
    let mut workflow = AgentSessionWorkflow::default();
    let stored = run_subscription("sub_stored", "token_stored", 1);
    let pending = run_subscription("sub_pending", "token_pending", 1);
    workflow
        .run_subscriptions
        .insert(stored.subscription_id.clone(), stored);
    workflow
        .pending_terminal_notifications
        .push(PendingRunTerminalNotification {
            subscription: pending,
            notification: terminal_notification("token_pending", 1, RunStatus::Completed),
        });

    workflow.unsubscribe_from_run("sub_stored");

    assert!(workflow.run_subscriptions.is_empty());
    assert_eq!(workflow.pending_terminal_notifications.len(), 1);

    workflow.unsubscribe_from_run("sub_pending");

    assert!(workflow.pending_terminal_notifications.is_empty());
}

#[test]
fn run_terminal_signal_records_active_wait_arrival_idempotently() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow
        .active_waits
        .insert(7, active_wait_record(7, "token_1"));
    let notification = terminal_notification("token_1", 1, RunStatus::Completed);

    workflow.record_run_terminal(notification.clone());
    workflow.record_run_terminal(notification);
    workflow.record_run_terminal(terminal_notification("other", 1, RunStatus::Completed));

    let wait = workflow.active_waits.get(&7).expect("active wait");
    assert_eq!(wait.results.len(), 1);
    assert_eq!(wait.results[0].status, AgentWaitHandleStatus::Terminal);
    assert_eq!(wait.results[0].target_session_id, "target_session");
    assert_eq!(wait.results[0].run_id, "run_1");
    assert_eq!(
        wait.results[0].run.as_ref().map(|run| run.status.as_str()),
        Some("completed")
    );
}

#[test]
fn all_mode_active_wait_resolves_after_all_child_notifications_arrive() {
    let mut wait = active_wait_record(7, "token_1");
    let second_target_session_id = SessionId::new("target_session_two");
    let second_run_id = RunId::new(2);
    wait.handles.push(crate::AgentWaitHandle {
        target_session_id: second_target_session_id.clone(),
        run_id: second_run_id,
    });
    wait.results.push(crate::AgentWaitHandleResult {
        target_session_id: second_target_session_id.as_str().to_owned(),
        run_id: fleet_waits::api_run_id(second_run_id),
        status: AgentWaitHandleStatus::Pending,
        run: None,
        error: None,
    });
    wait.subscriptions.push(ActiveWaitSubscription {
        target_session_id: second_target_session_id,
        subscription: RunSubscription {
            subscription_id: "sub_wait_two".to_owned(),
            subscriber_workflow_id: "subscriber_session".to_owned(),
            correlation_token: "token_2".to_owned(),
            run_id: second_run_id,
        },
    });

    let mut workflow = AgentSessionWorkflow::default();
    workflow.active_waits.insert(7, wait);
    assert_eq!(
        fleet_waits::active_wait_nontimer_resolution(
            workflow.active_waits.get(&7).expect("active wait")
        ),
        None
    );

    workflow.record_run_terminal(terminal_notification("token_1", 1, RunStatus::Completed));
    let wait = workflow.active_waits.get(&7).expect("active wait");
    assert_eq!(fleet_waits::active_wait_nontimer_resolution(wait), None);
    assert_eq!(
        wait.results
            .iter()
            .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
            .count(),
        1
    );

    workflow.record_run_terminal(terminal_notification("token_2", 2, RunStatus::Completed));
    let wait = workflow.active_waits.get(&7).expect("active wait");
    assert_eq!(
        fleet_waits::active_wait_nontimer_resolution(wait),
        Some(AgentWaitOutcome::Terminal)
    );
    assert!(
        wait.results
            .iter()
            .all(|result| result.status == AgentWaitHandleStatus::Terminal)
    );
}

#[test]
fn environment_job_wait_wake_hint_marks_matching_wait_due() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow
        .active_environment_job_waits
        .insert(9, active_environment_job_wait(9, 42_000));

    workflow.record_environment_job_changed(EnvironmentJobChanged {
        session_id: "session_1".to_owned(),
        env_id: "env_1".to_owned(),
        job_id: "job_1".to_owned(),
    });

    let wait = workflow
        .active_environment_job_waits
        .get(&9)
        .expect("active job wait");
    assert_eq!(wait.next_check_at_ms, 0);
    assert!(wait_loop::workflow_state_has_immediate_work(&workflow));
}

#[test]
fn environment_job_wait_advance_clamps_to_deadline() {
    let mut wait = active_environment_job_wait(9, 42_000);
    wait.deadline_ms = Some(43_000);

    job_waits::advance(&mut wait, 42_500);

    assert_eq!(wait.poll_attempt, 1);
    assert_eq!(wait.next_check_at_ms, 43_000);
}

#[test]
fn continue_as_new_is_blocked_by_waits_subscriptions_and_pending_work() {
    let mut workflow = AgentSessionWorkflow::default();
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));

    workflow.queue_admission(admission(request_run("submit_1")));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_admissions.clear();

    workflow.pending_tool_batch_resumes.push(pending_resume(1));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_tool_batch_resumes.clear();

    workflow
        .pending_terminal_notifications
        .push(PendingRunTerminalNotification {
            subscription: run_subscription("sub_pending", "token_pending", 1),
            notification: terminal_notification("token_pending", 1, RunStatus::Completed),
        });
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_terminal_notifications.clear();

    workflow
        .active_waits
        .insert(7, active_wait_record(7, "token_1"));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.active_waits.clear();

    workflow
        .active_environment_job_waits
        .insert(9, active_environment_job_wait(9, 42_000));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.active_environment_job_waits.clear();

    workflow.subscribe_to_run(run_subscription("sub_1", "token_1", 1));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.run_subscriptions.clear();

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
        .active_waits
        .insert(7, active_wait_record(7, "token_1"));
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.active_waits.clear();

    workflow
        .active_environment_job_waits
        .insert(9, active_environment_job_wait(9, 42_000));
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.active_environment_job_waits.clear();

    workflow.subscribe_to_run(run_subscription("sub_1", "token_1", 1));
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.run_subscriptions.clear();

    assert!(wait_loop::workflow_state_is_closed_and_quiescent(&workflow));
}

#[test]
fn close_on_terminal_requires_idle_open_session_with_completed_run() {
    let args = agent_session_args_with_close_on_terminal(true);
    let mut state = CoreAgentState::new();
    assert!(!drive::should_close_on_terminal(&args, &state));

    state.lifecycle.status = CoreAgentStatus::Open;
    assert!(!drive::should_close_on_terminal(&args, &state));

    state.runs.completed.push(RunRecord {
        run_id: RunId::new(1),
        status: RunStatus::Completed,
        submission_id: None,
        submission_digest: None,
        output_ref: None,
        failure: None,
    });
    assert!(drive::should_close_on_terminal(&args, &state));
    assert!(!drive::should_close_on_terminal(
        &agent_session_args_with_close_on_terminal(false),
        &state
    ));

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

fn request_run(submission_id: &str) -> CoreAgentCommand {
    CoreAgentCommand::RequestRun(engine::RunRequestCommand {
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
        context_key: None,
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
        instructions_ref: None,
        max_steps_per_input: None,
        continue_as_new_history_threshold: None,
        close_on_terminal,
    }
}

fn run_subscription(
    subscription_id: &str,
    correlation_token: &str,
    run_id: u64,
) -> RunSubscription {
    RunSubscription {
        subscription_id: subscription_id.to_owned(),
        subscriber_workflow_id: "subscriber_session".to_owned(),
        correlation_token: correlation_token.to_owned(),
        run_id: RunId::new(run_id),
    }
}

fn terminal_notification(
    correlation_token: &str,
    run_id: u64,
    status: RunStatus,
) -> RunTerminalNotification {
    RunTerminalNotification {
        correlation_token: correlation_token.to_owned(),
        run_id: RunId::new(run_id),
        status,
        output_ref: None,
        failure_message_ref: None,
    }
}

fn active_wait_record(batch_id: u64, correlation_token: &str) -> ActiveWaitRecord {
    let target_session_id = SessionId::new("target_session");
    let run_id = RunId::new(1);
    ActiveWaitRecord {
        batch_id: ToolBatchId::new(batch_id),
        run_id: RunId::new(10),
        turn_id: TurnId::new(20),
        call_id: engine::ToolCallId::new("call_wait"),
        mode: AgentWaitMode::All,
        handles: vec![crate::AgentWaitHandle {
            target_session_id: target_session_id.clone(),
            run_id,
        }],
        results: vec![crate::AgentWaitHandleResult {
            target_session_id: target_session_id.as_str().to_owned(),
            run_id: fleet_waits::api_run_id(run_id),
            status: AgentWaitHandleStatus::Pending,
            run: None,
            error: None,
        }],
        subscriptions: vec![ActiveWaitSubscription {
            target_session_id,
            subscription: RunSubscription {
                subscription_id: "sub_wait".to_owned(),
                subscriber_workflow_id: "subscriber_session".to_owned(),
                correlation_token: correlation_token.to_owned(),
                run_id,
            },
        }],
        deadline_ms: None,
    }
}

fn active_environment_job_wait(batch_id: u64, next_check_at_ms: u64) -> ActiveEnvironmentJobWait {
    ActiveEnvironmentJobWait {
        batch_id: ToolBatchId::new(batch_id),
        run_id: RunId::new(10),
        turn_id: TurnId::new(20),
        call_id: engine::ToolCallId::new("call_job_wait"),
        handles: vec![crate::EnvironmentJobHandle {
            session_id: "session_1".to_owned(),
            env_id: "env_1".to_owned(),
            job_id: "job_1".to_owned(),
        }],
        mode: crate::EnvironmentJobWaitMode::All,
        terminal_policy: crate::EnvironmentJobWaitTerminalPolicy::AnyTerminal,
        output_bytes: Some(1024),
        include_artifacts: false,
        deadline_ms: None,
        next_check_at_ms,
        poll_attempt: 0,
    }
}

fn pending_resume(batch_id: u64) -> PendingToolBatchResume {
    PendingToolBatchResume {
        batch_id: ToolBatchId::new(batch_id),
        result: ToolInvocationBatchResult {
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(batch_id),
            results: Vec::new(),
        },
    }
}

trait CommandSubmissionIdForTest {
    fn submission_id_for_test(&self) -> Option<SubmissionId>;
}

impl CommandSubmissionIdForTest for CoreAgentCommand {
    fn submission_id_for_test(&self) -> Option<SubmissionId> {
        drive::command_submission_id(self)
    }
}
