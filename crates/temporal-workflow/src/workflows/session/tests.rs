use super::*;
use harness::{
    AwaitSpec, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentEntry,
    CoreAgentJoins, EventSeq, PromiseScope, PromiseSource, PromiseStatus, RunId, RunRecord,
    RunStatus, RunTerminalNotifyIntent, SessionPosition, ToolBatchId, ToolCallId, TurnId,
};

#[test]
fn pending_admissions_are_fifo() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.queue_admission(admission(request_input_run("submit_1")));
    workflow.queue_admission(admission(request_input_run("submit_2")));

    let pending = std::mem::take(&mut workflow.pending_admissions);
    assert_eq!(
        pending[0].core().unwrap().command.submission_id_for_test(),
        Some(SubmissionId::new("submit_1"))
    );
    assert_eq!(
        pending[1].core().unwrap().command.submission_id_for_test(),
        Some(SubmissionId::new("submit_2"))
    );
}

#[test]
fn admission_failure_status_does_not_poison_later_admission() {
    let mut workflow = AgentSessionWorkflow::default();
    let rejection = harness::CommandRejection::context_revision_conflict(3, 4);
    workflow.admission_failures.push(AgentAdmissionFailure {
        preparation_error: None,
        submission_id: Some(SubmissionId::new("submit_rejected")),
        correlation_token: Some("admit_test".to_owned()),
        kind: AgentAdmissionFailureKind::RejectedCommand,
        message: rejection.to_string(),
        rejection: Some(rejection.clone()),
    });
    workflow.queue_admission(admission(request_input_run("submit_later")));

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
    assert_eq!(status.fatal_error, None);
}

#[test]
fn source_resolution_emission_queues_pending_resolution_with_producer() {
    let mut workflow = AgentSessionWorkflow::default();
    let payload_ref = harness::BlobRef::from_bytes(b"job output");
    workflow
        .queue_emission(
            test_universe(),
            harness::EmissionEnvelope::source_resolution(
                test_universe(),
                "universe/envjob-job_1".to_owned(),
                "universe/session_1",
                harness::PromiseId::new("promise_1"),
                harness::PromiseResolution::Resolved {
                    payload_ref: Some(payload_ref.clone()),
                },
            ),
        )
        .expect("queue resolution");

    // Source resolutions defer to the main loop for producer authorization
    // and optional reply-schema validation before ResolvePromise admission.
    assert!(workflow.pending_admissions.is_empty());
    let pending = &workflow.pending_source_resolutions[0];
    assert_eq!(pending.promise_id.as_str(), "promise_1");
    assert!(matches!(
        &pending.resolution,
        harness::PromiseResolution::Resolved {
            payload_ref: Some(actual),
        } if actual == &payload_ref
    ));
    assert!(matches!(
        &pending.producer,
        harness::EmissionProducer::Workflow { workflow_id, .. }
            if workflow_id == "universe/envjob-job_1"
    ));
}

#[test]
fn duplicate_source_resolution_delivery_is_an_end_to_end_noop() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.core_state.lifecycle.status = CoreAgentStatus::Open;
    workflow.core_state.promises.promises.insert(
        harness::PromiseId::new("promise_1"),
        promise("promise_1", PromiseStatus::Pending),
    );
    let payload_ref = harness::BlobRef::from_bytes(b"job output");
    let envelope = harness::EmissionEnvelope::source_resolution(
        test_universe(),
        "universe/envjob-job_1".to_owned(),
        "universe/session_1",
        harness::PromiseId::new("promise_1"),
        harness::PromiseResolution::Resolved {
            payload_ref: Some(payload_ref.clone()),
        },
    );
    workflow
        .queue_emission(test_universe(), envelope.clone())
        .expect("first delivery");
    workflow
        .queue_emission(test_universe(), envelope)
        .expect("second delivery");

    // Each delivery becomes an ordinary ResolvePromise admission here, skipping the
    // deferred authorization stage, so the harness's first writer decides.
    let pending = std::mem::take(&mut workflow.pending_source_resolutions);
    assert_eq!(pending.len(), 2);
    let admissions: Vec<_> = pending
        .into_iter()
        .map(|pending| crate::AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id: pending.promise_id,
                resolution: pending.resolution,
            },
            correlation_token: None,
        })
        .collect();
    assert_eq!(admissions.len(), 2);
    let mut appended = 0u64;
    for (index, admission) in admissions.into_iter().enumerate() {
        let proposals =
            harness::admit_command(&workflow.core_state, admission.command, index as u64 + 1)
                .expect("admit duplicate emission");
        appended += proposals.len() as u64;
        for proposal in proposals {
            let seq = workflow
                .core_state
                .reduced_to
                .as_ref()
                .map_or(1, |position| position.seq.as_u64() + 1);
            harness::apply_event(
                &mut workflow.core_state,
                &CoreAgentEntry {
                    position: SessionPosition {
                        seq: EventSeq::new(seq),
                    },
                    observed_at_ms: index as u64 + 1,
                    joins: proposal.joins,
                    event: proposal.event,
                },
            )
            .expect("apply first promise resolution");
        }
    }

    assert_eq!(appended, 1);
    let promise = workflow
        .core_state
        .promises
        .promises
        .get(&harness::PromiseId::new("promise_1"))
        .expect("resolved promise");
    assert_eq!(promise.status, PromiseStatus::Resolved);
    assert_eq!(promise.payload_ref.as_ref(), Some(&payload_ref));
}

#[test]
fn cross_universe_emission_is_rejected_before_admission() {
    let mut workflow = AgentSessionWorkflow::default();
    let producer_universe = uuid::Uuid::from_u128(2);
    let warning = workflow
        .queue_emission(
            test_universe(),
            harness::EmissionEnvelope::source_resolution(
                producer_universe,
                "other-universe/envjob-job_1".to_owned(),
                "universe/session_1",
                harness::PromiseId::new("promise_1"),
                harness::PromiseResolution::Resolved { payload_ref: None },
            ),
        )
        .expect_err("another universe's emission is refused");

    assert!(warning.contains("cross-universe emission rejected"));
    assert!(workflow.pending_admissions.is_empty());
    assert!(workflow.pending_source_resolutions.is_empty());
}

#[test]
fn a_warning_is_advisory_and_stays_out_of_the_fatal_error() {
    let mut workflow = AgentSessionWorkflow {
        session_id: Some(SessionId::new("session_1")),
        ..Default::default()
    };

    workflow.record_warning(true, "first".to_owned());
    workflow.record_warning(true, "unauthorized producer".to_owned());

    let status = workflow.status_snapshot();
    assert_eq!(status.fatal_error, None);
    assert!(!status.bootstrap_failed);
    assert_eq!(
        status.last_warning.as_deref(),
        Some("unauthorized producer")
    );
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
        submission_digest: 0,
        source: harness::RunSource::Input { input: Vec::new() },
        first_seq: EventSeq::new(1),
        terminal_seq: EventSeq::new(1),
        accepted_at_ms: 1,
        started_at_ms: Some(1),
        completed_at_ms: 1,
        usage: None,
        output: None,
        failure: None,
    });
    assert!(drive::should_close_on_terminal(&args, &state));
    assert!(!drive::should_close_on_terminal(
        &agent_session_args_with_close_on_terminal(false),
        &state
    ));

    state.promises.promises.insert(
        harness::PromiseId::new("promise_14"),
        promise("promise_14", PromiseStatus::Pending),
    );
    assert!(!drive::should_close_on_terminal(&args, &state));
    state
        .promises
        .promises
        .get_mut(&harness::PromiseId::new("promise_14"))
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

#[test]
fn a_due_rollover_continues_when_safe_and_yields_only_to_real_outer_work() {
    use drive::history_boundary_outcome_for as outcome;

    assert_eq!(outcome(false, true, true), None);
    assert_eq!(outcome(false, false, true), None);
    assert_eq!(
        outcome(true, true, false),
        Some(DriveOutcome::ContinueAsNew)
    );
    assert_eq!(outcome(true, true, true), Some(DriveOutcome::ContinueAsNew));
    assert_eq!(
        outcome(true, false, true),
        Some(DriveOutcome::YieldForWorkflowWork)
    );
    assert_eq!(
        outcome(true, false, false),
        None,
        "a blocker the outer loop cannot clear leaves the drive to carry on"
    );
}

#[test]
fn blockers_that_need_the_drive_or_the_clock_never_yield_to_the_outer_loop() {
    const NOW: u64 = 1_000;
    let unblocked = || AgentSessionWorkflow {
        ready: true,
        ..Default::default()
    };
    let blocked_and_not_clearable = |state: &AgentSessionWorkflow, blocker: &str| {
        assert!(
            !wait_loop::workflow_state_allows_continue_as_new(state),
            "{blocker} blocks continuation"
        );
        assert!(
            !wait_loop::outer_loop_can_clear_now(state, NOW),
            "{blocker} cannot be cleared by the outer loop now"
        );
    };

    let mut watchdog = unblocked();
    watchdog.cancelling_watchdog = Some(CancellingWatchdog {
        run_id: 1,
        since_ms: NOW,
    });
    blocked_and_not_clearable(&watchdog, "an armed watchdog");

    let mut backing_off = unblocked();
    let mut retrying = pending_run_emission();
    retrying.next_attempt_at_ms = NOW + 1;
    backing_off.pending_emissions.push(retrying);
    blocked_and_not_clearable(&backing_off, "an emission in backoff");

    let mut start_backoff = workflow_with_start_intent().workflow;
    start_backoff.ready = true;
    let invocation = start_backoff
        .core_state
        .workflow_tools
        .start_requests
        .keys()
        .next()
        .expect("start intent")
        .as_str()
        .to_owned();
    start_backoff
        .workflow_start_backoffs
        .insert(invocation, (1, NOW + 1));
    blocked_and_not_clearable(&start_backoff, "a start in backoff");

    let mut held = workflow_with_turn_in_flight();
    held.queue_admission(admission(CoreAgentCommand::RemoveContext {
        expected_revision: None,
        key: harness::ContextEntryKey::new("client.note"),
    }));
    assert!(!admissions::has_admissible_admissions(&held));
    blocked_and_not_clearable(&held, "an admission held behind a turn");
}

#[test]
fn work_the_outer_loop_consumes_with_an_await_makes_a_blocked_rollover_yield() {
    const NOW: u64 = 1_000;
    let mut workflow = AgentSessionWorkflow {
        ready: true,
        ..Default::default()
    };
    assert!(!wait_loop::outer_loop_can_clear_now(&workflow, NOW));

    workflow.queue_admission(admission(request_input_run("submit_1")));
    assert!(wait_loop::outer_loop_can_clear_now(&workflow, NOW));
    workflow.pending_admissions.clear();

    workflow.pending_tool_batch_resumes.push(pending_resume(1));
    assert!(wait_loop::outer_loop_can_clear_now(&workflow, NOW));
    workflow.pending_tool_batch_resumes.clear();

    workflow
        .pending_promise_cancellations
        .push(pending_promise_cancellation("promise_1"));
    assert!(wait_loop::outer_loop_can_clear_now(&workflow, NOW));
    workflow.pending_promise_cancellations.clear();

    // A retained emission counts once its retry is due, not only when it is new.
    let mut retrying = pending_run_emission();
    retrying.next_attempt_at_ms = NOW;
    workflow.pending_emissions.push(retrying);
    assert!(wait_loop::outer_loop_can_clear_now(&workflow, NOW));
    assert!(!wait_loop::outer_loop_can_clear_now(&workflow, NOW - 1));
}

#[test]
fn transport_work_is_due_until_a_service_pass_so_a_yield_cannot_repeat() {
    const NOW: u64 = 1_000;
    let mut workflow = AgentSessionWorkflow {
        ready: true,
        ..Default::default()
    };
    assert!(!wait_loop::transport_work_due(&workflow, NOW));

    // An emission: a failed attempt keeps it, with its retry ahead of the clock.
    workflow.pending_emissions.push(pending_run_emission());
    assert!(wait_loop::transport_work_due(&workflow, NOW));
    workflow.pending_emissions[0].next_attempt_at_ms = NOW + 2_000;
    assert!(!wait_loop::transport_work_due(&workflow, NOW));
    assert!(wait_loop::transport_work_due(&workflow, NOW + 2_000));
    workflow.pending_emissions.clear();

    // Queues the pass takes whole.
    workflow
        .pending_promise_cancellations
        .push(pending_promise_cancellation("promise_1"));
    assert!(wait_loop::transport_work_due(&workflow, NOW));
    workflow.pending_promise_cancellations.clear();
    workflow
        .pending_source_resolutions
        .push(PendingSourceResolution {
            promise_id: harness::PromiseId::new("promise_1"),
            resolution: harness::PromiseResolution::Resolved { payload_ref: None },
            producer: harness::EmissionProducer::Workflow {
                universe_id: test_universe(),
                workflow_id: "producer".to_owned(),
            },
        });
    assert!(wait_loop::transport_work_due(&workflow, NOW));
    workflow.pending_source_resolutions.clear();
    assert!(!wait_loop::transport_work_due(&workflow, NOW));

    // A start: a new intent is due, a failed attempt backs off, a started one is done.
    let StartIntent {
        mut workflow,
        invocation_id,
        promise_id,
        ..
    } = workflow_with_start_intent();
    assert!(wait_loop::transport_work_due(&workflow, NOW));
    let key = invocation_id.as_str().to_owned();
    workflow
        .workflow_start_backoffs
        .insert(key.clone(), (1, NOW + 2_000));
    assert!(!wait_loop::transport_work_due(&workflow, NOW));
    assert!(wait_loop::transport_work_due(&workflow, NOW + 2_000));
    workflow.workflow_start_backoffs.clear();
    workflow.confirmed_workflow_starts.insert(key);
    assert!(!wait_loop::transport_work_due(&workflow, NOW));

    // A deadline: due while the promise is pending, spent once it has failed.
    let promise = workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise");
    promise.deadline_ms = Some(NOW);
    assert!(wait_loop::transport_work_due(&workflow, NOW));
    assert!(!wait_loop::transport_work_due(&workflow, NOW - 1));
    workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise")
        .status = PromiseStatus::Failed;
    assert!(!wait_loop::transport_work_due(&workflow, NOW));
}

fn promise_entry(event: harness::PromiseEvent) -> CoreAgentEntry {
    CoreAgentEntry {
        position: SessionPosition {
            seq: harness::EventSeq::new(1),
        },
        observed_at_ms: 1,
        joins: Default::default(),
        event: CoreAgentEvent::Promise(event),
    }
}

#[test]
fn any_terminal_outcome_of_the_last_keyed_promise_queues_one_cancel() {
    let terminal = |promise_id: &harness::PromiseId| {
        [
            harness::PromiseEvent::Resolved {
                promise_id: promise_id.clone(),
                payload_ref: None,
            },
            harness::PromiseEvent::Failed {
                promise_id: promise_id.clone(),
                error_ref: None,
            },
            harness::PromiseEvent::Cancelled {
                promise_id: promise_id.clone(),
            },
        ]
    };
    let StartIntent {
        workflow: fixture,
        promise_id,
        execution_id,
        ..
    } = workflow_with_start_intent();

    for event in terminal(&promise_id) {
        let mut workflow = AgentSessionWorkflow {
            universe_id: fixture.universe_id,
            session_id: fixture.session_id.clone(),
            core_state: fixture.core_state.clone(),
            ..Default::default()
        };
        // Still pending in state: this entry did not end the set.
        workflow.queue_execution_cancel_for_entry(&promise_entry(event.clone()));
        assert!(workflow.pending_execution_cancels.is_empty());

        workflow
            .core_state
            .promises
            .promises
            .get_mut(&promise_id)
            .expect("promise")
            .status = PromiseStatus::Cancelled;
        workflow.queue_execution_cancel_for_entry(&promise_entry(event.clone()));
        workflow.queue_execution_cancel_for_entry(&promise_entry(event));
        assert_eq!(
            workflow.pending_execution_cancels,
            BTreeSet::from([execution_id.clone()]),
            "deduplicated by execution"
        );
        assert!(wait_loop::workflow_state_has_immediate_work(&workflow));
        assert!(wait_loop::transport_work_due(&workflow, 0));
    }
}

#[test]
fn a_promise_with_no_started_invocation_queues_no_cancel() {
    let mut workflow = AgentSessionWorkflow::default();
    add_promises(
        &mut workflow,
        vec![
            promise_with_source(
                "promise_1",
                PromiseStatus::Resolved,
                bound_workflow_source("peer"),
                PromiseScope::Session,
            ),
            promise("promise_2", PromiseStatus::Resolved),
        ],
    );
    for id in ["promise_1", "promise_2", "promise_99"] {
        workflow.queue_execution_cancel_for_entry(&promise_entry(
            harness::PromiseEvent::Resolved {
                promise_id: harness::PromiseId::new(id),
                payload_ref: None,
            },
        ));
    }
    assert!(workflow.pending_execution_cancels.is_empty());
}

#[test]
fn past_invocations_cost_a_continued_execution_nothing_and_queued_cancels_travel() {
    let StartIntent {
        mut workflow,
        promise_id,
        execution_id,
        ..
    } = workflow_with_start_intent();
    workflow.ready = true;
    workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise")
        .status = PromiseStatus::Resolved;

    // Finished long ago, so a fresh execution has nothing queued and nothing to do.
    assert!(!wait_loop::workflow_state_has_immediate_work(&workflow));
    assert!(!wait_loop::transport_work_due(&workflow, 1_000));

    // A queued cancel does not hold a rollover back: it goes along.
    workflow
        .pending_execution_cancels
        .insert(execution_id.clone());
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));
    let continuation = AgentSessionContinuationState {
        version: AgentSessionContinuationState::VERSION,
        ready: true,
        operation_outcomes: Default::default(),
        admission_failures: Vec::new(),
        pending_execution_cancels: workflow.pending_execution_cancels.clone(),
    };
    let restored: AgentSessionContinuationState =
        serde_json::from_slice(&serde_json::to_vec(&continuation).unwrap()).unwrap();
    assert_eq!(
        restored.pending_execution_cancels,
        BTreeSet::from([execution_id])
    );
}

#[test]
fn a_closing_session_sends_its_queued_cancels_before_it_completes() {
    let mut workflow = AgentSessionWorkflow {
        initialized: true,
        ..Default::default()
    };
    workflow.core_state.lifecycle.status = CoreAgentStatus::Closed;
    assert!(wait_loop::workflow_state_is_closed_and_quiescent(&workflow));
    workflow
        .pending_execution_cancels
        .insert("execution".to_owned());
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
}

#[test]
fn a_start_backoff_whose_candidate_is_gone_does_not_block_rollover_for_good() {
    let StartIntent {
        mut workflow,
        invocation_id,
        promise_id,
        ..
    } = workflow_with_start_intent();
    workflow.ready = true;
    let key = invocation_id.as_str().to_owned();
    workflow
        .workflow_start_backoffs
        .insert(key.clone(), (1, 2_000));
    workflow_starts::forget_stale_backoffs(&mut workflow);
    assert!(
        workflow.workflow_start_backoffs.contains_key(&key),
        "a live candidate keeps its backoff"
    );

    workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise")
        .status = PromiseStatus::Cancelled;
    workflow_starts::forget_stale_backoffs(&mut workflow);

    assert!(workflow.workflow_start_backoffs.is_empty());
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));
}

#[test]
fn blockers_that_already_ended_do_not_hold_a_rollover_through_a_long_drive() {
    let StartIntent {
        mut workflow,
        invocation_id,
        promise_id,
        ..
    } = workflow_with_start_intent();
    workflow.ready = true;
    workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise")
        .status = PromiseStatus::Cancelled;
    workflow
        .workflow_start_backoffs
        .insert(invocation_id.as_str().to_owned(), (1, 2_000));
    workflow.cancelling_watchdog = Some(CancellingWatchdog {
        run_id: 7,
        since_ms: 1_000,
    });
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    assert!(
        !wait_loop::transport_work_due(&workflow, 1_000)
            && !wait_loop::outer_loop_can_clear_now(&workflow, 1_000),
        "nothing would send this drive back to the outer loop"
    );

    wait_loop::forget_ended_rollover_blockers(&mut workflow, 1_000);

    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));
}

#[test]
fn a_loop_that_stops_awaiting_is_caught_and_a_moving_clock_is_not() {
    let mut guard = wait_loop::StallGuard::default();
    for now in 0..1_000 {
        assert!(!guard.stalled(1, now), "the clock moved");
    }
    for history_length in 2..1_000 {
        assert!(!guard.stalled(history_length, 1_000), "history moved");
    }
    let passes = (0..100).filter(|_| guard.stalled(1_000, 1_000)).count();
    assert!(
        (2..=4).contains(&passes),
        "a frozen activation reports repeatedly, got {passes}"
    );
}

#[test]
fn rehydrated_active_run_wakes_core_drive_without_a_new_signal() {
    let mut workflow = workflow_with_parked_tool_batch(AwaitSpec {
        promise_ids: vec![harness::PromiseId::new("promise_1")],
        mode: harness::AwaitMode::All,
        deadline_at_ms: Some(50_000),
    });
    assert!(!wait_loop::workflow_state_needs_core_drive_for_state(
        &workflow
    ));
    workflow
        .core_state
        .runs
        .active
        .as_mut()
        .expect("active run")
        .parked_tool_batch = None;
    assert!(wait_loop::workflow_state_needs_core_drive_for_state(
        &workflow
    ));
}

#[test]
fn only_commands_reissued_from_state_escape_a_rejected_admission() {
    let run_id = RunId::new(1);
    assert!(drive::reissued_from_state(
        &CoreAgentCommand::ForceCancelRun { run_id }
    ));
    assert!(drive::reissued_from_state(
        &CoreAgentCommand::ResumeToolBatch(harness::ResumeToolBatchCommand {
            run_id,
            batch_id: harness::ToolBatchId::new(1),
            claim: harness::WakeReason::Terminal,
            claim_observed_at_ms: 1,
            output: harness::ToolBatchResumeOutput::JoinedWorkflowCalls {
                additional_context: Vec::new(),
            },
        })
    ));
    assert!(!drive::reissued_from_state(&CoreAgentCommand::CancelRun {
        requested_by: None,
        run_id,
    }));
    assert!(!drive::reissued_from_state(&request_input_run("client")));
}

#[test]
fn a_deferred_toolset_waits_out_a_parked_batch_without_spinning_the_loop() {
    let mut workflow = workflow_with_parked_tool_batch(AwaitSpec {
        promise_ids: vec![harness::PromiseId::new("promise_1")],
        mode: harness::AwaitMode::All,
        deadline_at_ms: Some(50_000),
    });
    let config = crate::default_session_config(harness::ModelSelection {
        api_kind: harness::ProviderApiKind::OpenAiResponses,
        provider_id: "openai".to_owned(),
        model: "gpt-test".to_owned(),
    });
    workflow.pending_toolsets.push(preparation::PendingToolset {
        prepared: crate::SessionToolsetPreparation {
            source: crate::SessionToolsetSource {
                config,
                config_revision: 0,
                bindings: Default::default(),
                system_binding_ids: Default::default(),
            },
            declarations: Vec::new(),
            tools: Default::default(),
        },
        run_id: RunId::new(2),
        admission: admission(request_input_run("queued_behind_parked_batch")),
    });
    assert!(harness::tools_frozen(&workflow.core_state));
    assert!(!wait_loop::workflow_state_needs_core_drive_for_state(
        &workflow
    ));
    assert!(!wait_loop::workflow_state_has_immediate_work(&workflow));

    // Once the batch is gone the toolset publishes at the very next wake.
    let run = workflow.core_state.runs.active.as_mut().unwrap();
    run.parked_tool_batch = None;
    run.active_tool_batch_id = None;
    run.status = RunStatus::Active;
    assert!(!harness::tools_frozen(&workflow.core_state));
    assert!(wait_loop::workflow_state_needs_core_drive_for_state(
        &workflow
    ));
}

fn continuation_state(
    admission_failures: Vec<AgentAdmissionFailure>,
) -> AgentSessionContinuationState {
    AgentSessionContinuationState {
        version: AgentSessionContinuationState::VERSION,
        ready: false,
        operation_outcomes: crate::SessionOperationReceipts::default(),
        admission_failures,
        pending_execution_cancels: BTreeSet::from(["execution".to_owned()]),
    }
}

#[test]
fn continuation_state_round_trips_admission_failure_correlation() {
    let rejection = harness::CommandRejection::context_revision_conflict(3, 4);
    let continuation = continuation_state(vec![AgentAdmissionFailure {
        preparation_error: None,
        submission_id: Some(SubmissionId::new("submit_rejected")),
        correlation_token: Some("admit_test".to_owned()),
        kind: AgentAdmissionFailureKind::RejectedCommand,
        message: rejection.to_string(),
        rejection: Some(rejection),
    }]);
    let encoded = serde_json::to_vec(&continuation).expect("encode continuation state");
    let decoded: AgentSessionContinuationState =
        serde_json::from_slice(&encoded).expect("decode continuation state");
    assert_eq!(decoded, continuation);
}

/// An input-bearing run request with a submission id: the generic admission
/// used where a test only needs "some command carrying input".
fn request_input_run(submission_id: &str) -> CoreAgentCommand {
    CoreAgentCommand::RequestRun(harness::RunRequestCommand {
        requested_by: None,
        notify_on_terminal: Vec::new(),
        submission_id: Some(SubmissionId::new(submission_id)),
        source: harness::RunRequestSource::Input {
            input: user_input(harness::BlobRef::from_bytes(submission_id.as_bytes())),
        },
        run_config: crate::default_run_config(),
    })
}

fn user_input(content_ref: harness::BlobRef) -> Vec<ContextEntryInput> {
    vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: harness::ContentRef {
            content_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
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
        setup: None,
        metadata: Default::default(),
        universe_id: test_universe(),
        session_id: SessionId::new("session_test"),
        display_name: None,
        delete_after_close_ms: None,
        session_config: crate::default_session_config(harness::ModelSelection {
            api_kind: harness::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "gpt-test".to_owned(),
        }),
        workflow_tools: None,
        continue_as_new_history_threshold: None,
        close_on_terminal,
        auto_reject_approvals: false,
        continuation_state: None,
    }
}

fn test_universe() -> uuid::Uuid {
    uuid::Uuid::from_u128(1)
}

fn managed_workflow_tools(controller_workflow_id: &str) -> harness::ManagedSessionWorkflowTools {
    harness::ManagedSessionWorkflowTools::v1(
        Some(harness::WorkflowEndpointRef {
            workflow_id: controller_workflow_id.to_owned(),
            workflow_kind: "agent_work".to_owned(),
        }),
        Vec::new(),
    )
}

#[test]
fn bootstrap_creation_identity_records_source_universe_and_is_immutable() {
    let declaration = managed_workflow_tools("deployment-global work controller 🔧");
    bootstrap::validate_session_creation_identity(
        test_universe(),
        &CoreAgentState::new(),
        true,
        Some(&declaration),
    )
    .expect("fresh managed session with opaque controller id");

    let mut existing = CoreAgentState::new();
    existing.workflow_tools.session_universe_id = Some(test_universe());
    existing.workflow_tools.managed_creation_fingerprint = Some(
        declaration
            .creation_fingerprint(test_universe())
            .expect("creation fingerprint"),
    );
    bootstrap::validate_session_creation_identity(
        test_universe(),
        &existing,
        false,
        Some(&declaration),
    )
    .expect("matching restart");
    assert!(
        bootstrap::validate_session_creation_identity(
            test_universe(),
            &existing,
            false,
            Some(&managed_workflow_tools("another arbitrary controller id")),
        )
        .is_err()
    );
    assert!(
        bootstrap::validate_session_creation_identity(
            uuid::Uuid::from_u128(2),
            &existing,
            false,
            Some(&declaration),
        )
        .is_err()
    );
    assert!(
        bootstrap::validate_session_creation_identity(test_universe(), &existing, false, None)
            .is_err()
    );
}

fn pending_run_emission() -> PendingEmission {
    PendingEmission::immediate(
        "universe/parent".to_owned(),
        harness::EmissionEnvelope::run_terminal(
            test_universe(),
            SessionId::new("session_child"),
            EventSeq::new(1),
            "promise_1".to_owned(),
            RunId::new(1),
            RunStatus::Completed,
            None,
            None,
        ),
    )
}

fn pending_resume(batch_id: u64) -> PendingToolBatchResume {
    PendingToolBatchResume {
        batch_id: ToolBatchId::new(batch_id),
        command: harness::ResumeToolBatchCommand {
            run_id: RunId::new(1),
            batch_id: ToolBatchId::new(batch_id),
            claim: harness::WakeReason::Timeout,
            claim_observed_at_ms: 1_000,
            output: harness::ToolBatchResumeOutput::AwaitTool {
                result_ref: harness::BlobRef::from_bytes(b"await output"),
                additional_context: Vec::new(),
                attachments: Vec::new(),
            },
        },
    }
}

fn pending_promise_cancellation(promise_id: &str) -> PendingPromiseCancellation {
    PendingPromiseCancellation {
        promise_id: promise_id.to_owned(),
        source: PromiseSource::Timer { fire_at_ms: 1_000 },
        log_seq: 0,
    }
}

fn workflow_with_parked_tool_batch(spec: harness::AwaitSpec) -> AgentSessionWorkflow {
    let mut workflow = AgentSessionWorkflow {
        ready: true,
        setup_requested: false,
        ..Default::default()
    };
    let run_id = RunId::new(1);
    let turn_id = TurnId::new(1);
    let batch_id = ToolBatchId::new(1);
    let call_id = ToolCallId::new("call_await");
    let mut tool_batches = std::collections::BTreeMap::new();
    tool_batches.insert(
        batch_id,
        harness::ActiveToolBatch {
            batch_id,
            run_id,
            turn_id,
            promise_id_base: 1,
            calls: vec![harness::ToolCallState {
                call: harness::ObservedToolCall {
                    call_id: call_id.clone(),
                    tool_id: Some((harness::ToolName::new("await")).clone()),
                    tool_name: harness::ToolName::new("await"),
                    provider_kind: None,
                    arguments_ref: harness::BlobRef::from_bytes(b"{}"),
                    native_call_ref: None,
                },
                status: harness::ToolCallStatus::Pending,
                result: None,
            }],
        },
    );
    workflow.core_state.runs.active = Some(harness::ActiveRun {
        context_recovery: Default::default(),
        run_id,
        status: RunStatus::Parked,
        submission_id: None,
        source: harness::RunSource::Input {
            input: user_input(harness::BlobRef::from_bytes(b"start")),
        },
        input_entry_ids: Vec::new(),
        input_consumed_by_turn_id: None,
        run_config: crate::default_run_config(),
        config_revision: 0,
        first_seq: EventSeq::new(1),
        accepted_at_ms: 1,
        started_at_ms: Some(1),
        usage: None,
        steering: Vec::new(),
        turns: std::collections::BTreeMap::new(),
        turn_count: 0,
        active_turn_id: None,
        active_tool_batch_id: Some(batch_id),
        approvals: Default::default(),
        parked_tool_batch: Some(harness::ParkedToolBatch {
            batch_id,
            suspension: harness::ToolBatchSuspension::AwaitTool { call_id, spec },
        }),
        tool_batches,
        completed_tool_batches: std::collections::BTreeMap::new(),
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
fn run_terminal_notifications_cannot_drop_output_encoding_into_a_session_promise() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow
        .queue_emission(
            test_universe(),
            harness::EmissionEnvelope::run_terminal(
                test_universe(),
                SessionId::new("child_a"),
                EventSeq::new(8),
                "promise_1".to_owned(),
                RunId::new(1),
                RunStatus::Completed,
                Some(harness::ContentRef {
                    content_ref: harness::BlobRef::from_bytes(b"native output"),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("provider.message".to_owned()),
                }),
                None,
            ),
        )
        .expect_err("a session does not take run-terminal notifications");
    assert!(workflow.pending_admissions.is_empty());
    assert!(workflow.pending_source_resolutions.is_empty());
}

#[test]
fn bound_dispatch_controls_push_delivery_independently_of_completion() {
    let mut workflow = AgentSessionWorkflow {
        universe_id: Some(test_universe()),
        session_id: Some(SessionId::new("child_session")),
        ..Default::default()
    };

    let binding = harness::WorkflowToolBinding::admit(
        test_universe(),
        harness::WorkflowToolDefinition {
            tool_id: harness::WorkflowToolId::new("approve"),
            revision: 1,
            semantic_type: "lightspeed.approval.request.v1".to_owned(),
            tool: harness::ToolSpec {
                name: harness::ToolName::new("request_approval"),
                execution: Default::default(),
                kind: harness::ToolKind::Function(harness::FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: harness::BlobRef::from_bytes(b"{}"),
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
                parallelism: harness::ToolParallelism::ParallelSafe,
            },
        },
        harness::WorkflowToolTarget::Bound {
            receiver: harness::WorkflowEndpointRef {
                workflow_id: "approval plugin id".to_owned(),
                workflow_kind: "approvals".to_owned(),
            },
            dispatch: harness::BoundWorkflowToolDispatch::Push,
        },
        harness::WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms: None,
            max_promises: 1,
            key_source: harness::WorkflowToolCompletionKeySource::Reply,
        },
    )
    .expect("binding");
    let invocation_id = harness::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        harness::TurnId::new(1),
        ToolBatchId::new(1),
        &harness::ToolCallId::new("call-1"),
        &binding.binding_fingerprint,
    );
    let invocation = harness::WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: binding.definition.tool_id.clone(),
        semantic_type: binding.definition.semantic_type.clone(),
        schema_revision: 1,
        binding_fingerprint: binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: harness::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: harness::ToolCallId::new("call-1"),
        arguments_ref: harness::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: Some(std::collections::BTreeMap::from([(
            harness::REPLY_COMPLETION_KEY.to_owned(),
            harness::PromiseId::from_number(1),
        )])),
    };
    workflow
        .core_state
        .workflow_tools
        .bindings
        .insert(binding.definition.tool_id.clone(), binding.clone());

    let entry = harness::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(9),
        },
        observed_at_ms: 100,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::WorkflowTool(harness::WorkflowToolEvent::Emitted {
            invocation: invocation.clone(),
        }),
    };
    workflow
        .queue_emissions_for_entries(std::slice::from_ref(&entry))
        .expect("queue push delivery");

    assert_eq!(workflow.pending_emissions.len(), 1);
    let pending = &workflow.pending_emissions[0];
    assert_eq!(pending.receiver_workflow_id, "approval plugin id");
    assert_eq!(pending.attempts, 0);
    assert_eq!(pending.next_attempt_at_ms, 0);
    assert_eq!(
        pending.envelope.emission_id.as_str(),
        invocation_id.as_str()
    );
    assert!(matches!(
        &pending.envelope.body,
        harness::EmissionBody::ToolInvocation {
            invocation: delivered,
            ..
        }
            if delivered == &invocation
    ));

    // Accepted completion uses the same push path when dispatch says Push.
    let accepted_binding = harness::WorkflowToolBinding::admit(
        test_universe(),
        binding.definition.clone(),
        harness::WorkflowToolTarget::Bound {
            receiver: harness::WorkflowEndpointRef {
                workflow_id: "approval plugin id".to_owned(),
                workflow_kind: "approvals".to_owned(),
            },
            dispatch: harness::BoundWorkflowToolDispatch::Push,
        },
        harness::WorkflowToolCompletion::Accepted,
    )
    .expect("pushed Accepted binding");
    let accepted_invocation_id = harness::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        harness::TurnId::new(1),
        ToolBatchId::new(1),
        &harness::ToolCallId::new("call-2"),
        &accepted_binding.binding_fingerprint,
    );
    let accepted_invocation = harness::WorkflowToolInvocation {
        invocation_id: accepted_invocation_id.clone(),
        tool_id: accepted_binding.definition.tool_id.clone(),
        semantic_type: accepted_binding.definition.semantic_type.clone(),
        schema_revision: accepted_binding.definition.revision,
        binding_fingerprint: accepted_binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: harness::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: harness::ToolCallId::new("call-2"),
        arguments_ref: harness::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: None,
    };
    workflow.core_state.workflow_tools.bindings.insert(
        accepted_binding.definition.tool_id.clone(),
        accepted_binding.clone(),
    );
    let accepted_entry = harness::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(10),
        },
        observed_at_ms: 101,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::WorkflowTool(harness::WorkflowToolEvent::Emitted {
            invocation: accepted_invocation.clone(),
        }),
    };
    workflow
        .queue_emissions_for_entries(std::slice::from_ref(&accepted_entry))
        .expect("queue pushed Accepted entry");
    assert_eq!(workflow.pending_emissions.len(), 2);
    assert_eq!(
        workflow.pending_emissions[1].envelope.emission_id.as_str(),
        accepted_invocation_id.as_str()
    );

    // Caller run completion does not abandon an independently queued pushed
    // Accepted delivery.
    workflow.core_state.runs.completed.push(RunRecord {
        run_id: RunId::new(1),
        status: RunStatus::Completed,
        submission_id: None,
        submission_digest: 0,
        source: harness::RunSource::Input { input: Vec::new() },
        first_seq: EventSeq::new(1),
        terminal_seq: EventSeq::new(1),
        accepted_at_ms: 1,
        started_at_ms: Some(1),
        completed_at_ms: 1,
        usage: None,
        output: None,
        failure: None,
        notify_on_terminal: Vec::new(),
    });
    let terminal_entry = harness::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(11),
        },
        observed_at_ms: 102,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::Run(RunEvent::Completed {
            run_id: RunId::new(1),
            output: None,
        }),
    };
    workflow
        .queue_emissions_for_entries(std::slice::from_ref(&terminal_entry))
        .expect("retain pushed Accepted delivery after run terminal");
    assert_eq!(workflow.pending_emissions.len(), 2);

    // Pull dispatch remains out of the push queue even with the same
    // Accepted completion contract.
    let pull_binding = harness::WorkflowToolBinding::admit(
        test_universe(),
        binding.definition,
        harness::WorkflowToolTarget::Bound {
            receiver: harness::WorkflowEndpointRef {
                workflow_id: "approval plugin id".to_owned(),
                workflow_kind: "approvals".to_owned(),
            },
            dispatch: harness::BoundWorkflowToolDispatch::Pull,
        },
        harness::WorkflowToolCompletion::Accepted,
    )
    .expect("pull Accepted binding");
    let pull_invocation_id = harness::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        harness::TurnId::new(1),
        ToolBatchId::new(1),
        &harness::ToolCallId::new("call-3"),
        &pull_binding.binding_fingerprint,
    );
    let pull_invocation = harness::WorkflowToolInvocation {
        invocation_id: pull_invocation_id,
        tool_id: pull_binding.definition.tool_id.clone(),
        semantic_type: pull_binding.definition.semantic_type.clone(),
        schema_revision: pull_binding.definition.revision,
        binding_fingerprint: pull_binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: harness::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: harness::ToolCallId::new("call-3"),
        arguments_ref: harness::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: None,
    };
    workflow
        .core_state
        .workflow_tools
        .bindings
        .insert(pull_binding.definition.tool_id.clone(), pull_binding);
    let pull_entry = harness::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(12),
        },
        observed_at_ms: 103,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::WorkflowTool(harness::WorkflowToolEvent::Emitted {
            invocation: pull_invocation,
        }),
    };
    workflow
        .queue_emissions_for_entries(std::slice::from_ref(&pull_entry))
        .expect("leave pull entry out of push queue");
    assert_eq!(workflow.pending_emissions.len(), 2);
}

#[test]
fn start_intents_recompute_pending_start_work_from_durable_state() {
    let StartIntent {
        mut workflow,
        invocation_id,
        promise_id,
        ..
    } = workflow_with_start_intent();

    // A durable start intent with a pending keyed promise is immediate
    // start work — recomputed from state, so replay and continue-as-new
    // rebuild it without transport bookkeeping.
    assert!(workflow_starts::has_immediate_work(&workflow));

    // A confirmed start needs no re-issue until state changes.
    workflow
        .confirmed_workflow_starts
        .insert(invocation_id.as_str().to_owned());
    assert!(!workflow_starts::has_immediate_work(&workflow));
    workflow.confirmed_workflow_starts.clear();

    // Terminal start failure removes the candidate.
    workflow
        .core_state
        .workflow_tools
        .start_failures
        .insert(invocation_id.clone(), harness::BlobRef::from_bytes(b"err"));
    assert!(!workflow_starts::has_immediate_work(&workflow));
    workflow.core_state.workflow_tools.start_failures.clear();

    // A terminal promise set leaves nothing to start; started executions
    // get a recovery poll instead of a bound-receiver push.
    workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise")
        .status = PromiseStatus::Resolved;
    assert!(!workflow_starts::has_immediate_work(&workflow));

    // Recovery polls cover pending started-execution promises only.
    workflow
        .core_state
        .promises
        .promises
        .get_mut(&promise_id)
        .expect("promise")
        .status = PromiseStatus::Pending;
    promise_sources::reconcile_polls_for_state(&mut workflow, 1_000);
    let poll = workflow
        .promise_source_polls
        .get(promise_id.as_str())
        .expect("recovery poll for started execution");
    assert!(
        poll.next_check_at_ms > 1_000,
        "recovery poll is a slow backstop"
    );
}

/// A session holding one start-on-call invocation with its keyed promise pending.
struct StartIntent {
    workflow: AgentSessionWorkflow,
    invocation_id: harness::WorkflowToolInvocationId,
    promise_id: harness::PromiseId,
    execution_id: String,
}

fn workflow_with_start_intent() -> StartIntent {
    let mut workflow = AgentSessionWorkflow {
        universe_id: Some(test_universe()),
        session_id: Some(SessionId::new("child_session")),
        ..Default::default()
    };

    let start = harness::WorkflowStartRef {
        recipe_format: 1,
        revision: 1,
        recipe_ref: harness::BlobRef::from_bytes(b"recipe"),
        recipe_fingerprint: "wtr:sha256:recipe".to_owned(),
    };
    let binding = harness::WorkflowToolBinding::admit(
        test_universe(),
        harness::WorkflowToolDefinition {
            tool_id: harness::WorkflowToolId::new("launch"),
            revision: 1,
            semantic_type: "lightspeed.job.launch.v1".to_owned(),
            tool: harness::ToolSpec {
                name: harness::ToolName::new("launch_job"),
                execution: Default::default(),
                kind: harness::ToolKind::Function(harness::FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: harness::BlobRef::from_bytes(b"{}"),
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
                parallelism: harness::ToolParallelism::ParallelSafe,
            },
        },
        harness::WorkflowToolTarget::Start {
            start: start.clone(),
        },
        harness::WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms: None,
            max_promises: 1,
            key_source: harness::WorkflowToolCompletionKeySource::Reply,
        },
    )
    .expect("start binding");
    let invocation_id = harness::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        harness::TurnId::new(1),
        ToolBatchId::new(1),
        &harness::ToolCallId::new("call-1"),
        &binding.binding_fingerprint,
    );
    let promise_id = harness::PromiseId::from_number(1);
    let execution_id =
        harness::workflow_tool_execution_id(&invocation_id, &start.recipe_fingerprint);
    let invocation = harness::WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: binding.definition.tool_id.clone(),
        semantic_type: binding.definition.semantic_type.clone(),
        schema_revision: 1,
        binding_fingerprint: binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: harness::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: harness::ToolCallId::new("call-1"),
        arguments_ref: harness::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: Some(std::collections::BTreeMap::from([(
            harness::REPLY_COMPLETION_KEY.to_owned(),
            promise_id.clone(),
        )])),
    };
    workflow
        .core_state
        .workflow_tools
        .bindings
        .insert(binding.definition.tool_id.clone(), binding);
    workflow
        .core_state
        .workflow_tools
        .start_requests
        .insert(invocation_id.clone(), invocation);
    workflow.core_state.promises.promises.insert(
        promise_id.clone(),
        harness::Promise {
            promise_id: promise_id.clone(),
            source: harness::PromiseSource::Workflow {
                producer_workflow_id: execution_id.clone(),
                producer_workflow_kind: harness::WORKFLOW_TOOL_EXECUTION_KIND.to_owned(),
                invocation_id: invocation_id.as_str().to_owned(),
                completion_key: harness::REPLY_COMPLETION_KEY.to_owned(),
            },
            scope: harness::PromiseScope::Session,
            ownership: harness::PromiseOwnership::Model,
            status: PromiseStatus::Pending,
            payload_ref: None,
            error_ref: None,
            deadline_ms: None,
        },
    );
    StartIntent {
        workflow,
        invocation_id,
        promise_id,
        execution_id,
    }
}

/// A ready session with an active run whose turn has started.
fn workflow_with_turn_in_flight() -> AgentSessionWorkflow {
    let mut workflow = AgentSessionWorkflow {
        ready: true,
        ..Default::default()
    };
    let mut drive = CoreAgentDrive::from_replayed(SessionId::new("held"), CoreAgentState::new());
    let commit = |drive: &mut CoreAgentDrive, action: CoreAgentAction| {
        let CoreAgentAction::AppendEvents { events, .. } = action else {
            panic!("expected an append, got {action:?}");
        };
        let mut seq = drive.head().map_or(0, |head| head.seq.as_u64());
        let entries = events
            .into_iter()
            .map(|event| {
                seq += 1;
                harness::storage::StoredSessionEntry {
                    position: SessionPosition {
                        seq: harness::EventSeq::new(seq),
                    },
                    observed_at_ms: event.observed_at_ms,
                    joins: event.joins,
                    event: event.event,
                }
            })
            .collect();
        drive.resume_appended(entries).expect("apply");
    };
    let open = drive
        .admit_command(
            CoreAgentCommand::OpenSession {
                config: crate::default_session_config(harness::ModelSelection {
                    api_kind: harness::ProviderApiKind::OpenAiResponses,
                    provider_id: "openai".into(),
                    model: "gpt-test".into(),
                }),
            },
            1,
        )
        .expect("open");
    commit(&mut drive, open);
    let run = drive
        .admit_command(request_input_run("submit_held"), 2)
        .expect("run");
    commit(&mut drive, run);
    for now in 3..64 {
        match drive.next_action_unbounded(now).expect("next action") {
            CoreAgentAction::GenerateLlm { .. } => {
                workflow.core_state = drive.state().clone();
                return workflow;
            }
            action => commit(&mut drive, action),
        }
    }
    panic!("the run never reached its generation");
}

#[test]
fn terminal_run_with_notify_intent_queues_emission() {
    let mut workflow = AgentSessionWorkflow {
        universe_id: Some(test_universe()),
        session_id: Some(SessionId::new("child_session")),
        ..Default::default()
    };
    let output_ref = harness::ContentRef::text(harness::BlobRef::from_bytes(b"done"));
    workflow.core_state.runs.completed.push(RunRecord {
        run_id: RunId::new(3),
        status: RunStatus::Completed,
        submission_id: None,
        submission_digest: 0,
        source: harness::RunSource::Input { input: Vec::new() },
        first_seq: EventSeq::new(1),
        terminal_seq: EventSeq::new(1),
        accepted_at_ms: 1,
        started_at_ms: Some(1),
        completed_at_ms: 1,
        usage: None,
        output: Some(output_ref.clone()),
        failure: None,
        notify_on_terminal: vec![RunTerminalNotifyIntent {
            holder_workflow_id: "universe/parent_session".to_owned(),
            token: "promise_parent".to_owned(),
        }],
    });
    let entry = harness::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(1),
        },
        observed_at_ms: 100,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::Run(RunEvent::Completed {
            run_id: RunId::new(3),
            output: Some(output_ref.clone()),
        }),
    };

    workflow
        .queue_emissions_for_entries(std::slice::from_ref(&entry))
        .expect("queue emission");

    assert_eq!(workflow.pending_emissions.len(), 1);
    let pending = &workflow.pending_emissions[0];
    assert_eq!(pending.receiver_workflow_id, "universe/parent_session");
    assert!(matches!(
        &pending.envelope.producer,
        harness::EmissionProducer::Session {
            universe_id,
            session_id,
            log_seq,
        } if *universe_id == test_universe()
            && session_id == &SessionId::new("child_session")
            && *log_seq == EventSeq::new(1)
    ));
    assert!(matches!(
        &pending.envelope.body,
        harness::EmissionBody::RunTerminal {
            token,
            run_id,
            status: RunStatus::Completed,
            output: Some(actual),
            failure_message_ref: None,
        } if token == "promise_parent"
            && *run_id == RunId::new(3)
            && actual == &output_ref
    ));
    // A run without intents queues nothing.
    workflow.pending_emissions.clear();
    workflow.core_state.runs.completed[0]
        .notify_on_terminal
        .clear();
    workflow
        .queue_emissions_for_entries(std::slice::from_ref(&entry))
        .expect("queue no emission");
    assert!(workflow.pending_emissions.is_empty());
}

#[test]
fn a_receiver_hears_of_a_hard_deadline_but_not_its_own_failure_report() {
    let mut workflow = workflow_holding(vec![
        promise("promise_1", PromiseStatus::Failed),
        promise("promise_2", PromiseStatus::Failed),
        promise("promise_3", PromiseStatus::Failed),
        promise("promise_4", PromiseStatus::Cancelled),
    ]);
    let failed = |id: &str, error_ref| {
        promise_entry(harness::PromiseEvent::Failed {
            promise_id: harness::PromiseId::new(id),
            error_ref,
        })
    };
    for entry in [
        failed("promise_1", Some(harness::promise_deadline_exceeded_ref())),
        failed(
            "promise_2",
            Some(harness::BlobRef::from_bytes(b"its error")),
        ),
        failed("promise_3", None),
        promise_entry(harness::PromiseEvent::Cancelled {
            promise_id: harness::PromiseId::new("promise_4"),
        }),
    ] {
        workflow.queue_promise_cancellation_for_entry(&entry);
    }
    let notified = workflow
        .pending_promise_cancellations
        .iter()
        .map(|pending| pending.promise_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(notified, ["promise_1", "promise_4"]);
}

fn delivery_from(producer: &str, promise_id: &str) -> PendingSourceResolution {
    PendingSourceResolution {
        promise_id: harness::PromiseId::new(promise_id),
        resolution: harness::PromiseResolution::Resolved {
            payload_ref: Some(harness::BlobRef::from_bytes(b"forged")),
        },
        producer: harness::EmissionProducer::Workflow {
            universe_id: test_universe(),
            workflow_id: producer.to_owned(),
        },
    }
}

fn workflow_holding(promises: Vec<harness::Promise>) -> AgentSessionWorkflow {
    let mut workflow = AgentSessionWorkflow::default();
    add_promises(&mut workflow, promises);
    workflow
}

#[test]
fn a_late_or_unknown_source_resolution_is_a_no_op_that_never_fails_the_session() {
    let mut workflow = workflow_holding(vec![promise("promise_1", PromiseStatus::Resolved)]);

    // A pruned promise looks unknown, and its late reply still reports nothing fatal.
    assert_eq!(
        promise_sources::source_resolution_gate(
            &mut workflow,
            false,
            &delivery_from("universe/child", "promise_9"),
        ),
        promise_sources::SourceResolutionGate::Drop
    );
    assert!(
        workflow
            .last_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("promise_9"))
    );
    assert!(workflow.fatal_error.is_none());
    assert!(workflow.admission_failures.is_empty());

    // The first writer won on a terminal promise, which is the designed path.
    workflow.last_warning = None;
    assert_eq!(
        promise_sources::source_resolution_gate(
            &mut workflow,
            false,
            &delivery_from("universe/child", "promise_1"),
        ),
        promise_sources::SourceResolutionGate::Drop
    );
    assert!(workflow.last_warning.is_none());
    assert!(workflow.pending_admissions.is_empty());
}

/// What a recovery poll found arrives from the promise's own producer and goes through
/// the gate a signal does, so the binding's reply schema applies.
#[test]
fn a_recovered_result_meets_the_reply_schema_a_signal_would() {
    let StartIntent {
        mut workflow,
        promise_id,
        execution_id,
        ..
    } = workflow_with_start_intent();
    let schema = harness::BlobRef::from_bytes(b"{}");
    workflow
        .core_state
        .workflow_tools
        .bindings
        .get_mut(&harness::WorkflowToolId::new("launch"))
        .expect("binding")
        .completion = harness::WorkflowToolCompletion::Promises {
        reply_schema_ref: Some(schema.clone()),
        deadline_after_ms: None,
        max_promises: 1,
        key_source: harness::WorkflowToolCompletionKeySource::Reply,
    };
    assert_eq!(
        promise_sources::source_resolution_gate(
            &mut workflow,
            false,
            &delivery_from(&execution_id, promise_id.as_str()),
        ),
        promise_sources::SourceResolutionGate::Admit {
            reply_schema_ref: Some(schema),
            text_reply: false,
        }
    );
    assert!(workflow.last_warning.is_none());
}

/// A joined reply becomes its call's result, which providers read as text.
#[test]
fn a_joined_binding_asks_the_ingest_check_for_a_text_reply() {
    let StartIntent {
        mut workflow,
        promise_id,
        execution_id,
        ..
    } = workflow_with_start_intent();
    workflow
        .core_state
        .workflow_tools
        .bindings
        .get_mut(&harness::WorkflowToolId::new("launch"))
        .expect("binding")
        .completion = harness::WorkflowToolCompletion::Joined {
        reply_schema_ref: None,
        deadline_after_ms: 1_000,
    };
    assert_eq!(
        promise_sources::source_resolution_gate(
            &mut workflow,
            false,
            &delivery_from(&execution_id, promise_id.as_str()),
        ),
        promise_sources::SourceResolutionGate::Admit {
            reply_schema_ref: None,
            text_reply: true,
        }
    );
}

#[test]
fn the_ingest_check_runs_for_every_ref_and_every_declared_schema() {
    let schema = harness::BlobRef::from_bytes(b"{}");
    let blob = harness::BlobRef::from_bytes(b"reply");
    let needs = promise_sources::resolution_needs_ingest_check;
    assert!(needs(
        None,
        &harness::PromiseResolution::Resolved {
            payload_ref: Some(blob.clone())
        }
    ));
    assert!(needs(
        None,
        &harness::PromiseResolution::Failed {
            error_ref: Some(blob)
        }
    ));
    // A schema requires a payload, so its absence is for the check to report.
    assert!(needs(
        Some(&schema),
        &harness::PromiseResolution::Resolved { payload_ref: None }
    ));
    assert!(!needs(
        None,
        &harness::PromiseResolution::Resolved { payload_ref: None }
    ));
    assert!(!needs(
        Some(&schema),
        &harness::PromiseResolution::Failed { error_ref: None }
    ));
    assert!(!needs(
        Some(&schema),
        &harness::PromiseResolution::Cancelled
    ));
}

#[test]
fn only_the_stored_producer_settles_a_promise_and_nothing_outside_settles_a_timer() {
    let timer = promise_with_source(
        "promise_2",
        PromiseStatus::Pending,
        PromiseSource::Timer { fire_at_ms: 1_000 },
        PromiseScope::Session,
    );
    let mut workflow = workflow_holding(vec![promise("promise_1", PromiseStatus::Pending), timer]);

    for delivery in [
        delivery_from("universe/other", "promise_1"),
        delivery_from("universe/child", "promise_2"),
    ] {
        workflow.last_warning = None;
        assert_eq!(
            promise_sources::source_resolution_gate(&mut workflow, false, &delivery),
            promise_sources::SourceResolutionGate::Drop
        );
        assert!(
            workflow
                .last_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("unauthorized producer"))
        );
    }
    assert_eq!(
        promise_sources::source_resolution_gate(
            &mut workflow,
            false,
            &delivery_from("universe/child", "promise_1"),
        ),
        promise_sources::SourceResolutionGate::Admit {
            reply_schema_ref: None,
            text_reply: false,
        }
    );
}

fn promise(id: &str, status: PromiseStatus) -> harness::Promise {
    promise_with_source(
        id,
        status,
        bound_workflow_source("child"),
        PromiseScope::Session,
    )
}

/// A bound-receiver workflow promise: resolved only by pushed emission, so
/// it never enters the recovery poll set.
fn bound_workflow_source(peer: &str) -> PromiseSource {
    PromiseSource::Workflow {
        producer_workflow_id: format!("universe/{peer}"),
        producer_workflow_kind: "bound_receiver".to_owned(),
        invocation_id: format!("wti_{peer}"),
        completion_key: harness::REPLY_COMPLETION_KEY.to_owned(),
    }
}

fn promise_with_source(
    id: &str,
    status: PromiseStatus,
    source: PromiseSource,
    scope: PromiseScope,
) -> harness::Promise {
    harness::Promise {
        promise_id: harness::PromiseId::new(id),
        source,
        scope,
        ownership: harness::PromiseOwnership::Model,
        status,
        payload_ref: None,
        error_ref: None,
        deadline_ms: None,
    }
}

fn add_promises(workflow: &mut AgentSessionWorkflow, promises: Vec<harness::Promise>) {
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
    mode: harness::AwaitMode,
    deadline_at_ms: Option<u64>,
) -> harness::AwaitSpec {
    harness::AwaitSpec {
        promise_ids: ids.iter().map(|id| harness::PromiseId::new(*id)).collect(),
        mode,
        deadline_at_ms,
    }
}

#[test]
fn workflow_await_waits_for_every_promise_in_all_mode() {
    let mut workflow = workflow_with_parked_tool_batch(await_spec(
        &["promise_1", "promise_2"],
        harness::AwaitMode::All,
        None,
    ));
    add_promises(
        &mut workflow,
        vec![
            promise("promise_1", PromiseStatus::Resolved),
            promise("promise_2", PromiseStatus::Pending),
        ],
    );
    assert!(!awaits::has_satisfied_await(&workflow));

    add_promises(
        &mut workflow,
        vec![
            promise("promise_1", PromiseStatus::Resolved),
            promise("promise_2", PromiseStatus::Failed),
        ],
    );
    assert!(awaits::has_satisfied_await(&workflow));
}

#[test]
fn parked_join_reconstructs_wake_from_runtime_owned_terminal_promises() {
    let promise_id = harness::PromiseId::new("promise_15");
    let mut workflow = workflow_with_parked_tool_batch(harness::AwaitSpec {
        promise_ids: vec![promise_id.clone()],
        mode: harness::AwaitMode::All,
        deadline_at_ms: None,
    });
    let invocation_id = harness::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        TurnId::new(1),
        ToolBatchId::new(1),
        &ToolCallId::new("call_await"),
        "wtb:sha256:test",
    );
    workflow
        .core_state
        .runs
        .active
        .as_mut()
        .expect("active run")
        .parked_tool_batch
        .as_mut()
        .expect("parked batch")
        .suspension = harness::ToolBatchSuspension::JoinedWorkflowCalls {
        calls: vec![harness::JoinedWorkflowCall {
            call_id: ToolCallId::new("call_await"),
            invocation_id,
            promise_id: promise_id.clone(),
        }],
        spec: harness::AwaitSpec {
            promise_ids: vec![promise_id.clone()],
            mode: harness::AwaitMode::All,
            deadline_at_ms: None,
        },
    };
    workflow.core_state.promises.promises.insert(
        promise_id.clone(),
        harness::Promise {
            promise_id,
            source: PromiseSource::Workflow {
                producer_workflow_id: "channels".to_owned(),
                producer_workflow_kind: "channels.session".to_owned(),
                invocation_id: "joined".to_owned(),
                completion_key: harness::REPLY_COMPLETION_KEY.to_owned(),
            },
            scope: PromiseScope::Run {
                run_id: RunId::new(1),
            },
            ownership: harness::PromiseOwnership::Runtime,
            status: PromiseStatus::Resolved,
            payload_ref: Some(harness::BlobRef::from_bytes(b"receipt")),
            error_ref: None,
            deadline_ms: Some(50_000),
        },
    );

    assert!(awaits::has_satisfied_await(&workflow));
    assert!(wait_loop::workflow_state_has_immediate_work(&workflow));
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));
}

#[test]
fn workflow_await_resolves_any_mode_on_first_terminal_promise() {
    let mut workflow = workflow_with_parked_tool_batch(await_spec(
        &["promise_1", "promise_2"],
        harness::AwaitMode::Any,
        None,
    ));
    add_promises(
        &mut workflow,
        vec![
            promise("promise_1", PromiseStatus::Cancelled),
            promise("promise_2", PromiseStatus::Pending),
        ],
    );
    assert!(awaits::has_satisfied_await(&workflow));
}

#[test]
fn workflow_await_deadline_uses_timer_not_state_condition() {
    let mut workflow = workflow_with_parked_tool_batch(await_spec(
        &["promise_1"],
        harness::AwaitMode::All,
        Some(1_000),
    ));
    add_promises(
        &mut workflow,
        vec![promise("promise_1", PromiseStatus::Pending)],
    );
    assert!(!awaits::has_satisfied_await(&workflow));
    assert_eq!(awaits::nearest_await_wake_ms(&workflow), Some(1_000));
}

#[test]
fn queued_followups_do_not_spin_while_the_active_run_awaits_a_timer() {
    let mut workflow =
        workflow_with_parked_tool_batch(await_spec(&["promise_1"], harness::AwaitMode::All, None));
    add_promises(
        &mut workflow,
        vec![promise_with_source(
            "promise_1",
            PromiseStatus::Pending,
            PromiseSource::Timer { fire_at_ms: 11_000 },
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        )],
    );
    let active = workflow.core_state.runs.active.as_ref().unwrap();
    for run_id in [2, 3] {
        workflow.core_state.runs.queued.push(harness::AcceptedRun {
            run_id: RunId::new(run_id),
            submission_id: None,
            source: active.source.clone(),
            run_config: active.run_config.clone(),
            config_revision: active.config_revision,
            first_seq: EventSeq::new(run_id),
            accepted_at_ms: 1_000,
            notify_on_terminal: Vec::new(),
        });
    }
    promise_sources::reconcile_polls_for_state(&mut workflow, 1_000);

    // Without yielding, workflow time cannot advance and the durable timer
    // is never scheduled. Queued runs must wait for the active run to finish.
    assert!(!wait_loop::workflow_state_needs_core_drive_for_state(
        &workflow
    ));
    assert!(!wait_loop::workflow_state_has_immediate_work(&workflow));
    assert_eq!(promise_sources::nearest_wake_ms(&workflow), Some(11_000));

    // New controls can still wake the owner while the run is parked.
    workflow.queue_admission(admission(request_input_run("another_followup")));
    assert!(wait_loop::workflow_state_has_immediate_work(&workflow));
    workflow.pending_admissions.clear();

    workflow
        .core_state
        .promises
        .promises
        .get_mut(&harness::PromiseId::new("promise_1"))
        .unwrap()
        .status = PromiseStatus::Resolved;
    assert!(awaits::has_satisfied_await(&workflow));
    assert!(wait_loop::workflow_state_has_immediate_work(&workflow));

    // Once the active run finishes, the queued work must start immediately.
    workflow.core_state.runs.active = None;
    assert!(wait_loop::workflow_state_needs_core_drive_for_state(
        &workflow
    ));
    assert!(wait_loop::workflow_state_has_immediate_work(&workflow));
}

#[test]
fn promise_snapshot_reports_pending_promise() {
    let spec = await_spec(&["promise_1"], harness::AwaitMode::All, Some(1_000));
    let mut workflow = workflow_with_parked_tool_batch(spec.clone());
    add_promises(
        &mut workflow,
        vec![promise("promise_1", PromiseStatus::Pending)],
    );
    let snapshot = awaits::promise_snapshot(&spec, &workflow.core_state);
    assert_eq!(snapshot[0].status, "pending");
}

#[test]
fn continue_as_new_allows_pending_sources_and_parked_tool_batches() {
    let mut workflow = workflow_with_parked_tool_batch(harness::AwaitSpec {
        promise_ids: vec![
            harness::PromiseId::new("promise_11"),
            harness::PromiseId::new("promise_12"),
            harness::PromiseId::new("promise_13"),
            harness::PromiseId::new("promise_14"),
        ],
        mode: harness::AwaitMode::All,
        deadline_at_ms: Some(50_000),
    });
    let promises = [
        promise_with_source(
            "promise_11",
            PromiseStatus::Pending,
            bound_workflow_source("child"),
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "promise_12",
            PromiseStatus::Pending,
            bound_workflow_source("peer"),
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "promise_13",
            PromiseStatus::Pending,
            PromiseSource::Timer { fire_at_ms: 60_000 },
            PromiseScope::Run {
                run_id: RunId::new(1),
            },
        ),
        promise_with_source(
            "promise_14",
            PromiseStatus::Pending,
            bound_workflow_source("detached_child"),
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

    let parked = awaits::parked_tool_batch(&workflow.core_state).expect("parked await");
    assert_eq!(parked.spec().promise_ids.len(), 4);
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
    let timer_source = PromiseSource::Timer { fire_at_ms: 60_000 };
    let promises = [
        promise_with_source(
            "promise_13",
            PromiseStatus::Pending,
            timer_source.clone(),
            PromiseScope::Session,
        ),
        promise_with_source(
            "promise_11",
            PromiseStatus::Pending,
            bound_workflow_source("child"),
            PromiseScope::Session,
        ),
        promise_with_source(
            "promise_12",
            PromiseStatus::Pending,
            bound_workflow_source("peer"),
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
    assert!(!workflow.promise_source_polls.contains_key("promise_11"));
    assert!(!workflow.promise_source_polls.contains_key("promise_12"));
    let timer_poll = workflow
        .promise_source_polls
        .get("promise_13")
        .expect("timer poll");
    assert_eq!(timer_poll.source, timer_source);
    assert_eq!(timer_poll.next_check_at_ms, 60_000);
    assert_eq!(timer_poll.poll_attempt, 0);
    assert_eq!(promise_sources::nearest_wake_ms(&workflow), Some(60_000));
}

#[test]
fn a_started_execution_poll_backs_off_to_a_cap_and_a_timer_keeps_its_time() {
    let mut poll = PromiseSourcePoll {
        promise_id: "promise_1".to_owned(),
        source: bound_workflow_source("child"),
        next_check_at_ms: 0,
        poll_attempt: 0,
    };
    let delays = (0..6)
        .map(|_| {
            promise_sources::advance(&mut poll, 1_000);
            poll.next_check_at_ms - 1_000
        })
        .collect::<Vec<_>>();
    assert_eq!(delays, [20_000, 40_000, 60_000, 60_000, 60_000, 60_000]);
    poll.poll_attempt = u32::MAX;
    promise_sources::advance(&mut poll, u64::MAX);
    assert_eq!(poll.next_check_at_ms, u64::MAX);

    let mut timer = PromiseSourcePoll {
        promise_id: "promise_2".to_owned(),
        source: PromiseSource::Timer { fire_at_ms: 9_000 },
        next_check_at_ms: 0,
        poll_attempt: 0,
    };
    promise_sources::advance(&mut timer, 1_000);
    assert_eq!(timer.next_check_at_ms, 9_000);
}

#[test]
fn continue_as_new_is_blocked_by_non_reconstructible_workflow_state() {
    let mut workflow = AgentSessionWorkflow {
        ready: true,
        ..Default::default()
    };
    assert!(wait_loop::workflow_state_allows_continue_as_new(&workflow));

    workflow.queue_admission(admission(request_input_run("submit_1")));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_admissions.clear();

    workflow.pending_tool_batch_resumes.push(pending_resume(1));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_tool_batch_resumes.clear();

    workflow.pending_emissions.push(pending_run_emission());
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_emissions.clear();

    workflow
        .pending_promise_cancellations
        .push(pending_promise_cancellation("promise_1"));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.pending_promise_cancellations.clear();

    workflow
        .workflow_start_backoffs
        .insert("inv_1".to_owned(), (1, 100));
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.workflow_start_backoffs.clear();

    workflow.cancelling_watchdog = Some(CancellingWatchdog {
        run_id: 1,
        since_ms: 100,
    });
    assert!(!wait_loop::workflow_state_allows_continue_as_new(&workflow));
    workflow.cancelling_watchdog = None;

    // Log-derived state (pending promises) never blocks continue-as-new.
    workflow.core_state.promises.promises.insert(
        harness::PromiseId::new("promise_1"),
        promise("promise_1", PromiseStatus::Pending),
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

    workflow.pending_emissions.push(pending_run_emission());
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.pending_emissions.clear();

    workflow
        .pending_promise_cancellations
        .push(pending_promise_cancellation("promise_1"));
    assert!(!wait_loop::workflow_state_is_closed_and_quiescent(
        &workflow
    ));
    workflow.pending_promise_cancellations.clear();

    assert!(wait_loop::workflow_state_is_closed_and_quiescent(&workflow));
}

#[test]
fn attachment_catalog_survives_selection_changes_and_replays() {
    fn append(
        state: &mut CoreAgentState,
        log: &mut Vec<CoreAgentEntry>,
        command: CoreAgentCommand,
    ) {
        for proposal in harness::admit_command(state, command, 1).unwrap() {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            harness::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    fn publication(key: &str, origin: Option<String>) -> CoreAgentCommand {
        CoreAgentCommand::UpsertContext {
            expected_revision: None,
            key: ContextEntryKey::new(key),
            entry: ContextEntryInput {
                origin,
                kind: ContextEntryKind::Catalog {
                    title: "Catalog".into(),
                },
                content: harness::ContentRef::text(BlobRef::from_bytes(b"catalog")),
                preview: None,
                provenance_ref: None,
                token_estimate: None,
            },
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.environments = Some(harness::EnvironmentsFeature {
        environments: ["first", "second"]
            .into_iter()
            .map(|id| harness::EnvironmentAttachment {
                environment_id: id.into(),
                default: false,
                access: harness::EnvironmentAccess::Read,
                working_directory: None,
            })
            .collect(),
        ..Default::default()
    });
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::OpenSession { config },
    );
    append(
        &mut state,
        &mut log,
        publication("runtime.catalog.vfs", None),
    );
    let vfs = harness::current_context_entry(&state, &ContextEntryKey::new("runtime.catalog.vfs"))
        .unwrap()
        .clone();
    let tools = state.tooling.clone();
    let key = ContextEntryKey::new("runtime.catalog.environments");
    for next in [Some("first"), Some("second"), None] {
        let observed = publication(key.as_str(), Some("runtime.environments".into()));
        assert!(!drive::environment_attachment_catalog_publication_is_obsolete(&state, &observed));
        append(&mut state, &mut log, observed.clone());
        let before = harness::current_context_entry(&state, &key)
            .unwrap()
            .clone();
        let command = match next {
            Some(id) => CoreAgentCommand::SetActiveEnvironment {
                environment_id: harness::EnvironmentId::new(id),
            },
            None => CoreAgentCommand::ClearActiveEnvironment,
        };
        append(&mut state, &mut log, command);
        assert!(!drive::environment_attachment_catalog_publication_is_obsolete(&state, &observed));
        assert!(drive::invalid_environment_attachment_catalog_command(&state).is_none());
        assert_eq!(harness::current_context_entry(&state, &key), Some(&before));
        assert_eq!(
            harness::current_context_entry(&state, &ContextEntryKey::new("runtime.catalog.vfs")),
            Some(&vfs)
        );
        assert_eq!(state.tooling, tools);
    }
    let unselected = publication(key.as_str(), Some("runtime.environments".into()));
    let mut config = state.lifecycle.config.clone().unwrap();
    config.features.environments = None;
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::ReplaceSessionConfig {
            expected_revision: None,
            config,
        },
    );
    assert!(drive::environment_attachment_catalog_publication_is_obsolete(&state, &unselected));
    let removal = drive::invalid_environment_attachment_catalog_command(&state).unwrap();
    append(&mut state, &mut log, removal);
    let mut replayed = CoreAgentState::new();
    for entry in &log {
        harness::apply_event(&mut replayed, entry).unwrap();
    }
    assert_eq!(state, replayed);
}

#[test]
fn environment_catalog_switch_removal_replays_without_mutating_vfs() {
    fn append(
        state: &mut CoreAgentState,
        log: &mut Vec<CoreAgentEntry>,
        command: CoreAgentCommand,
    ) {
        for proposal in harness::admit_command(state, command, 1).unwrap() {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            harness::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.environments = Some(harness::EnvironmentsFeature {
        skills: Some(Default::default()),
        environments: ["first", "second"]
            .into_iter()
            .map(|id| harness::EnvironmentAttachment {
                environment_id: id.to_owned(),
                default: false,
                access: harness::EnvironmentAccess::Read,
                working_directory: None,
            })
            .collect(),
        ..Default::default()
    });
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::OpenSession { config },
    );
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::SetActiveEnvironment {
            environment_id: harness::EnvironmentId::new("first"),
        },
    );
    // The gateway can submit the first run before any skill catalog exists.
    assert!(state.context.entries.is_empty());
    assert!(
        admissions::should_refresh_runtime_projection_before_admitting(
            &state,
            &request_input_run("without-gateway-discovery"),
        )
    );
    let vfs_key = ContextEntryKey::new("runtime.catalog.skills.vfs");
    let env_key = ContextEntryKey::new("runtime.catalog.skills.environment");
    for (key, origin) in [
        (vfs_key.clone(), None),
        (
            env_key.clone(),
            Some("runtime.environment:first".to_owned()),
        ),
    ] {
        append(
            &mut state,
            &mut log,
            CoreAgentCommand::UpsertContext {
                expected_revision: None,
                key,
                entry: ContextEntryInput {
                    origin,
                    kind: ContextEntryKind::Catalog {
                        title: "Skills".into(),
                    },
                    content: harness::ContentRef::text(BlobRef::from_bytes(b"menu")),
                    preview: None,
                    provenance_ref: None,
                    token_estimate: None,
                },
            },
        );
    }
    let vfs = harness::current_context_entry(&state, &vfs_key)
        .unwrap()
        .clone();
    let environment_publication = CoreAgentCommand::UpsertContext {
        expected_revision: None,
        key: env_key.clone(),
        entry: harness::current_catalog_inputs(&state)[&env_key].clone(),
    };
    assert!(!drive::environment_catalog_publication_is_obsolete(
        &state,
        &environment_publication
    ));
    let mut disabled = state.clone();
    disabled
        .lifecycle
        .config
        .as_mut()
        .unwrap()
        .features
        .environments
        .as_mut()
        .unwrap()
        .skills = None;
    assert!(drive::invalid_environment_catalog_command(&disabled).is_some());
    assert_eq!(
        harness::current_context_entry(&disabled, &vfs_key),
        Some(&vfs)
    );
    disabled
        .lifecycle
        .config
        .as_mut()
        .unwrap()
        .features
        .environments
        .as_mut()
        .unwrap()
        .skills = Some(Default::default());
    disabled.lifecycle.config.as_mut().unwrap().features.vfs = None;
    assert!(drive::invalid_environment_catalog_command(&disabled).is_none());

    // A run submitted without gateway discovery must refresh even when the
    // previously published catalog is still valid: its source may have changed.
    assert!(drive::invalid_environment_catalog_command(&state).is_none());
    assert!(
        admissions::should_refresh_runtime_projection_before_admitting(
            &state,
            &request_input_run("idle")
        )
    );
    append(&mut state, &mut log, request_input_run("queued"));
    assert!(drive::environment_catalog_publication_is_obsolete(
        &state,
        &environment_publication
    ));
    assert!(
        !admissions::should_refresh_runtime_projection_before_admitting(
            &state,
            &request_input_run("already-queued")
        )
    );
    // The same boundary handles API selection and recorded selection tool effects.
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::SetActiveEnvironment {
            environment_id: harness::EnvironmentId::new("second"),
        },
    );
    let removal = drive::invalid_environment_catalog_command(&state).unwrap();
    assert!(matches!(&removal, CoreAgentCommand::RemoveContext { key, .. } if key == &env_key));
    assert!(!admissions::should_refresh_runtime_projection_before_admitting(&state, &removal));
    append(&mut state, &mut log, removal);
    assert!(harness::current_context_entry(&state, &env_key).is_none());
    assert_eq!(harness::current_context_entry(&state, &vfs_key), Some(&vfs));
    let mut replayed = CoreAgentState::new();
    for entry in &log {
        harness::apply_event(&mut replayed, entry).unwrap();
    }
    assert_eq!(state, replayed);
}

#[test]
fn environment_prompt_switch_removal_replays_without_mutating_vfs() {
    fn append(
        state: &mut CoreAgentState,
        log: &mut Vec<CoreAgentEntry>,
        command: CoreAgentCommand,
    ) {
        for proposal in harness::admit_command(state, command, 1).unwrap() {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            harness::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.environments = Some(harness::EnvironmentsFeature {
        prompts: Some(Default::default()),
        environments: ["first", "second"]
            .into_iter()
            .map(|id| harness::EnvironmentAttachment {
                environment_id: id.to_owned(),
                default: false,
                access: harness::EnvironmentAccess::Read,
                working_directory: None,
            })
            .collect(),
        ..Default::default()
    });
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::OpenSession { config },
    );
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::SetActiveEnvironment {
            environment_id: harness::EnvironmentId::new("first"),
        },
    );
    // Prompt discovery also runs when the gateway has not published instructions.
    assert!(state.context.entries.is_empty());
    assert!(
        admissions::should_refresh_runtime_projection_before_admitting(
            &state,
            &request_input_run("without-gateway-discovery"),
        )
    );
    let vfs_key = ContextEntryKey::new("instructions.100.prompts.test");
    let env_key = ContextEntryKey::new("instructions.110.environment");
    for (key, origin) in [
        (vfs_key.clone(), None),
        (
            env_key.clone(),
            Some("runtime.environment:first".to_owned()),
        ),
    ] {
        append(
            &mut state,
            &mut log,
            CoreAgentCommand::UpsertContext {
                expected_revision: None,
                key,
                entry: ContextEntryInput {
                    origin,
                    kind: ContextEntryKind::Instructions,
                    content: harness::ContentRef::text(BlobRef::from_bytes(b"menu")),
                    preview: None,
                    provenance_ref: None,
                    token_estimate: None,
                },
            },
        );
    }
    let vfs = harness::current_context_entry(&state, &vfs_key)
        .unwrap()
        .clone();
    let mut disabled = state.clone();
    disabled
        .lifecycle
        .config
        .as_mut()
        .unwrap()
        .features
        .environments
        .as_mut()
        .unwrap()
        .prompts = None;
    assert!(drive::invalid_environment_prompt_command(&disabled).is_some());
    assert_eq!(
        harness::current_context_entry(&disabled, &vfs_key),
        Some(&vfs)
    );
    disabled
        .lifecycle
        .config
        .as_mut()
        .unwrap()
        .features
        .environments
        .as_mut()
        .unwrap()
        .prompts = Some(Default::default());
    disabled.lifecycle.config.as_mut().unwrap().features.vfs = None;
    assert!(drive::invalid_environment_prompt_command(&disabled).is_none());

    assert!(drive::invalid_environment_prompt_command(&state).is_none());
    assert!(
        admissions::should_refresh_runtime_projection_before_admitting(
            &state,
            &request_input_run("idle")
        )
    );
    append(&mut state, &mut log, request_input_run("queued"));
    assert!(
        !admissions::should_refresh_runtime_projection_before_admitting(
            &state,
            &request_input_run("already-queued")
        )
    );
    // The same boundary handles API selection and recorded selection tool effects.
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::SetActiveEnvironment {
            environment_id: harness::EnvironmentId::new("second"),
        },
    );
    let removal = drive::invalid_environment_prompt_command(&state).unwrap();
    assert!(matches!(&removal, CoreAgentCommand::RemoveContext { key, .. } if key == &env_key));
    assert!(!admissions::should_refresh_runtime_projection_before_admitting(&state, &removal));
    append(&mut state, &mut log, removal);
    assert!(harness::current_context_entry(&state, &env_key).is_none());
    assert_eq!(harness::current_context_entry(&state, &vfs_key), Some(&vfs));
    let mut replayed = CoreAgentState::new();
    for entry in &log {
        harness::apply_event(&mut replayed, entry).unwrap();
    }
    assert_eq!(state, replayed);
}

#[test]
fn catalog_discovery_is_never_scheduled_for_continuation_or_tool_controls() {
    let mut state = CoreAgentState::new();
    state.lifecycle.status = CoreAgentStatus::Open;
    for command in [
        CoreAgentCommand::ClearActiveEnvironment,
        CoreAgentCommand::SetActiveEnvironment {
            environment_id: harness::EnvironmentId::new("machine"),
        },
        CoreAgentCommand::RemoveContext {
            expected_revision: None,
            key: ContextEntryKey::new("runtime.catalog.skills.environment"),
        },
    ] {
        assert!(!admissions::should_refresh_runtime_projection_before_admitting(&state, &command));
    }
}

#[test]
fn vfs_skill_revocation_is_source_scoped_and_replays() {
    fn append(
        state: &mut CoreAgentState,
        log: &mut Vec<CoreAgentEntry>,
        command: CoreAgentCommand,
    ) {
        let proposals = harness::admit_command(state, command, 1).unwrap();
        for proposal in proposals {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            harness::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.vfs = Some(harness::VfsFeature {
        workspaces: vec![harness::WorkspaceAttachment {
            path: "/skills".into(),
            target: harness::WorkspaceAttachmentTarget::Workspace {
                workspace_id: "skills".into(),
            },
            access: harness::WorkspaceAccess::Read,
        }],
        skills: Some(harness::VfsSkillsConfig::default()),
        ..Default::default()
    });
    config.features.environments = Some(harness::EnvironmentsFeature {
        skills: Some(Default::default()),
        ..Default::default()
    });
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::OpenSession { config },
    );
    let key = ContextEntryKey::new("runtime.catalog.skills.vfs");
    let env_key = ContextEntryKey::new("runtime.catalog.skills.environment");
    let entry = ContextEntryInput {
        origin: Some("runtime.vfs.skills".into()),
        kind: ContextEntryKind::Catalog {
            title: "VFS skills".into(),
        },
        content: harness::ContentRef::text(BlobRef::from_bytes(b"menu")),
        preview: None,
        provenance_ref: None,
        token_estimate: None,
    };
    let publication = CoreAgentCommand::UpsertContext {
        expected_revision: None,
        key: key.clone(),
        entry: entry.clone(),
    };
    assert!(!drive::vfs_skill_catalog_publication_is_obsolete(
        &state,
        &publication
    ));
    append(&mut state, &mut log, publication.clone());
    let mut environment_entry = entry;
    environment_entry.origin = None;
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::UpsertContext {
            expected_revision: None,
            key: env_key.clone(),
            entry: environment_entry,
        },
    );
    let environment = harness::current_context_entry(&state, &env_key)
        .unwrap()
        .clone();
    assert!(drive::invalid_vfs_skill_catalog_command(&state).is_none());
    let mut queued = state.clone();
    let mut queued_log = log.clone();
    append(&mut queued, &mut queued_log, request_input_run("queued"));
    assert!(drive::vfs_skill_catalog_publication_is_obsolete(
        &queued,
        &publication
    ));
    assert!(drive::invalid_vfs_skill_catalog_command(&queued).is_none());
    let mut config = state.lifecycle.config.clone().unwrap();
    config.features.vfs.as_mut().unwrap().skills = None;
    append(
        &mut state,
        &mut log,
        CoreAgentCommand::ReplaceSessionConfig {
            expected_revision: None,
            config,
        },
    );
    assert!(drive::vfs_skill_catalog_publication_is_obsolete(
        &state,
        &publication
    ));
    let removal = drive::invalid_vfs_skill_catalog_command(&state).unwrap();
    append(&mut state, &mut log, removal);
    assert!(harness::current_context_entry(&state, &key).is_none());
    assert_eq!(
        harness::current_context_entry(&state, &env_key),
        Some(&environment)
    );
    assert!(
        state
            .lifecycle
            .config
            .as_ref()
            .unwrap()
            .features
            .environments
            .as_ref()
            .unwrap()
            .skills
            .is_some()
    );
    let mut replayed = CoreAgentState::new();
    for entry in &log {
        harness::apply_event(&mut replayed, entry).unwrap();
    }
    assert_eq!(state, replayed);
    let mut external = match publication {
        CoreAgentCommand::UpsertContext { entry, .. } => entry,
        _ => unreachable!(),
    };
    external.origin = Some("controller".into());
    let command = CoreAgentCommand::UpsertContext {
        expected_revision: None,
        key,
        entry: external,
    };
    assert!(!drive::vfs_skill_catalog_publication_is_obsolete(
        &state, &command
    ));
    append(&mut state, &mut log, command);
    assert!(drive::invalid_vfs_skill_catalog_command(&state).is_none());
}

#[test]
fn accepted_submission_skips_new_policy_observations_even_after_config_is_removed() {
    let mut workflow =
        workflow_with_parked_tool_batch(await_spec(&["promise_1"], harness::AwaitMode::All, None));
    workflow
        .core_state
        .runs
        .active
        .as_mut()
        .unwrap()
        .submission_id = Some(SubmissionId::new("accepted"));
    workflow.core_state.lifecycle.config = None;
    assert!(preparation::known_submission(
        &workflow.core_state,
        &request_input_run("accepted")
    ));
    assert!(!preparation::known_submission(
        &workflow.core_state,
        &request_input_run("new")
    ));
}

#[test]
fn continuations_require_explicit_preparation_state() {
    for ready in [false, true] {
        let mut current = continuation_state(Vec::new());
        current.ready = ready;
        let wire = serde_json::to_value(&current).unwrap();
        assert_eq!(
            serde_json::from_value::<AgentSessionContinuationState>(wire.clone()).unwrap(),
            current
        );
        for field in ["ready", "operation_outcomes"] {
            let mut incomplete = wire.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<AgentSessionContinuationState>(incomplete).is_err());
        }
    }
}

#[test]
fn only_a_forced_close_passes_a_pending_compaction() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.core_state.context.compaction.phase =
        harness::ContextCompactionPhase::Pending(harness::ContextCompactionPlan {
            run_id: None,
            covered_entry_ids: Vec::new(),
            trigger: harness::ContextCompactionTrigger::Manual,
        });
    let close = |force| {
        SessionAdmission::Core(AgentAdmission {
            command: CoreAgentCommand::CloseSession { force },
            correlation_token: None,
        })
    };
    workflow.pending_admissions = vec![close(false)];
    assert!(!admissions::has_admissible_admissions(&workflow));
    workflow.pending_admissions = vec![close(true)];
    assert!(admissions::has_admissible_admissions(&workflow));
}

/// A batch or run ending in the append that settles a promise prunes what the host
/// has to read about that promise, so each entry is read before the next applies.
#[test]
fn a_terminal_event_and_a_pruning_boundary_in_one_append_still_reach_the_host() {
    let StartIntent {
        mut workflow,
        invocation_id,
        promise_id,
        execution_id,
    } = workflow_with_start_intent();
    workflow.core_state.runs.active = workflow_with_parked_tool_batch(await_spec(
        &[promise_id.as_str()],
        harness::AwaitMode::All,
        None,
    ))
    .core_state
    .runs
    .active;
    workflow.core_state.reduced_to = None;
    let entry = |seq, event| CoreAgentEntry {
        position: SessionPosition {
            seq: harness::EventSeq::new(seq),
        },
        observed_at_ms: 1,
        joins: Default::default(),
        event,
    };

    for appended in [
        entry(
            1,
            CoreAgentEvent::Promise(harness::PromiseEvent::Cancelled {
                promise_id: promise_id.clone(),
            }),
        ),
        entry(
            2,
            CoreAgentEvent::Run(harness::RunEvent::ForceCancelled {
                run_id: RunId::new(1),
            }),
        ),
    ] {
        drive::apply_appended_entry(&mut workflow, &appended).expect("apply");
    }

    assert_eq!(
        workflow.pending_execution_cancels,
        BTreeSet::from([execution_id])
    );
    assert_eq!(workflow.pending_promise_cancellations.len(), 1);
    assert_eq!(
        workflow.pending_promise_cancellations[0].promise_id,
        promise_id.as_str()
    );
    let state = &workflow.core_state;
    assert!(
        !state
            .workflow_tools
            .start_requests
            .contains_key(&invocation_id)
    );
}

/// The sibling settled long ago and left state, which says nothing against the
/// invocation's promises all being terminal.
#[test]
fn the_last_promise_of_an_invocation_cancels_its_execution_with_a_sibling_already_pruned() {
    let StartIntent {
        mut workflow,
        invocation_id,
        promise_id,
        execution_id,
    } = workflow_with_start_intent();
    workflow
        .core_state
        .workflow_tools
        .start_requests
        .get_mut(&invocation_id)
        .expect("start intent")
        .completion_promises
        .as_mut()
        .expect("keyed promises")
        .insert("pruned".to_owned(), harness::PromiseId::from_number(9_999));
    workflow.core_state.reduced_to = None;
    drive::apply_appended_entry(
        &mut workflow,
        &promise_entry(harness::PromiseEvent::Cancelled {
            promise_id: promise_id.clone(),
        }),
    )
    .expect("apply");
    assert_eq!(
        workflow.pending_execution_cancels,
        BTreeSet::from([execution_id])
    );
}

#[test]
fn a_runner_closing_after_its_scope_left_state_reads_it_as_closed() {
    let workflow = AgentSessionWorkflow::default();
    let report = code_tools::closed_report(&workflow, "execution").expect("report");
    assert!(report.closed);
    assert!(report.calls.is_empty());
    assert_eq!(report.execution_id, "execution");
}

/// A runner that lost its prepare receipt opens and closes the scope on this rejection,
/// which is what stops a delayed prepare opening it later.
#[test]
fn a_query_does_not_read_a_scope_never_opened_as_closed() {
    let workflow = AgentSessionWorkflow::default();
    let error = code_tools::report(&workflow, "execution").expect_err("unknown");
    assert_eq!(error.kind, crate::CodeToolRejectionKind::UnknownScope);
}
