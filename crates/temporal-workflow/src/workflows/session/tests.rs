use super::*;
use engine::{
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
    assert_eq!(status.last_error, None);
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
                content: engine::ContentRef {
                    content_ref: engine::BlobRef::from_bytes(b"audio"),
                    media_type: Some("audio/ogg".to_owned()),
                    provider_kind: None,
                },
                preview: Some("[audio]".to_owned()),
                origin: None,
                provenance_ref: None,
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
fn source_resolution_emission_queues_pending_resolution_with_producer() {
    let mut workflow = AgentSessionWorkflow::default();
    let payload_ref = engine::BlobRef::from_bytes(b"job output");
    workflow.queue_emission(
        test_universe(),
        engine::EmissionEnvelope::source_resolution(
            test_universe(),
            "universe/envjob-job_1".to_owned(),
            "universe/session_1",
            engine::PromiseId::new("promise_1"),
            engine::PromiseResolution::Resolved {
                payload_ref: Some(payload_ref.clone()),
            },
        ),
    );

    // Source resolutions defer to the main loop for producer authorization
    // and optional reply-schema validation before ResolvePromise admission.
    assert!(workflow.pending_admissions.is_empty());
    let pending = &workflow.pending_source_resolutions[0];
    assert_eq!(pending.promise_id.as_str(), "promise_1");
    assert!(matches!(
        &pending.resolution,
        engine::PromiseResolution::Resolved {
            payload_ref: Some(actual),
        } if actual == &payload_ref
    ));
    assert!(matches!(
        &pending.producer,
        engine::EmissionProducer::Workflow { workflow_id, .. }
            if workflow_id == "universe/envjob-job_1"
    ));
}

#[test]
fn duplicate_source_resolution_delivery_is_an_end_to_end_noop() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.core_state.lifecycle.status = CoreAgentStatus::Open;
    workflow.core_state.promises.promises.insert(
        engine::PromiseId::new("promise_1"),
        promise("promise_1", PromiseStatus::Pending),
    );
    let payload_ref = engine::BlobRef::from_bytes(b"job output");
    let envelope = engine::EmissionEnvelope::source_resolution(
        test_universe(),
        "universe/envjob-job_1".to_owned(),
        "universe/session_1",
        engine::PromiseId::new("promise_1"),
        engine::PromiseResolution::Resolved {
            payload_ref: Some(payload_ref.clone()),
        },
    );
    workflow.queue_emission(test_universe(), envelope.clone());
    workflow.queue_emission(test_universe(), envelope);

    // Both deliveries pass through the deferred authorization stage; a
    // non-workflow promise source needs no producer check or reply schema,
    // so each becomes an ordinary ResolvePromise admission.
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
            engine::admit_command(&workflow.core_state, admission.command, index as u64 + 1)
                .expect("admit duplicate emission");
        appended += proposals.len() as u64;
        for proposal in proposals {
            let seq = workflow
                .core_state
                .reduced_to
                .as_ref()
                .map_or(1, |position| position.seq.as_u64() + 1);
            engine::apply_event(
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
        .get(&engine::PromiseId::new("promise_1"))
        .expect("resolved promise");
    assert_eq!(promise.status, PromiseStatus::Resolved);
    assert_eq!(promise.payload_ref.as_ref(), Some(&payload_ref));
}

#[test]
fn cross_universe_emission_is_rejected_before_admission() {
    let mut workflow = AgentSessionWorkflow::default();
    let producer_universe = uuid::Uuid::from_u128(2);
    workflow.queue_emission(
        test_universe(),
        engine::EmissionEnvelope::source_resolution(
            producer_universe,
            "other-universe/envjob-job_1".to_owned(),
            "universe/session_1",
            engine::PromiseId::new("promise_1"),
            engine::PromiseResolution::Resolved { payload_ref: None },
        ),
    );

    assert!(workflow.pending_admissions.is_empty());
    assert!(
        workflow
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("cross-universe emission rejected"))
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
        submission_digest: None,
        source: engine::RunSource::Input { input: Vec::new() },
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
        engine::PromiseId::new("promise_14"),
        promise("promise_14", PromiseStatus::Pending),
    );
    assert!(!drive::should_close_on_terminal(&args, &state));
    state
        .promises
        .promises
        .get_mut(&engine::PromiseId::new("promise_14"))
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
fn active_drive_rollover_outcome_is_typed_and_waits_for_transport_drain() {
    assert_eq!(drive::history_boundary_outcome_for(false, true), None);
    assert_eq!(
        drive::history_boundary_outcome_for(true, true),
        Some(DriveOutcome::ContinueAsNew)
    );
    assert_eq!(
        drive::history_boundary_outcome_for(true, false),
        Some(DriveOutcome::YieldForWorkflowWork)
    );
}

#[test]
fn rehydrated_active_run_wakes_core_drive_without_a_new_signal() {
    let mut workflow = workflow_with_parked_tool_batch(AwaitSpec {
        promise_ids: vec![engine::PromiseId::new("promise_1")],
        mode: engine::AwaitMode::All,
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
fn legacy_step_limit_decodes_but_is_never_serialized() {
    let args = agent_session_args_with_close_on_terminal(false);
    let mut encoded = serde_json::to_value(&args).expect("serialize workflow args");
    assert!(encoded.get("max_steps_per_input").is_none());

    encoded
        .as_object_mut()
        .expect("workflow args object")
        .insert("max_steps_per_input".to_owned(), serde_json::json!(128));
    let decoded: AgentSessionArgs =
        serde_json::from_value(encoded).expect("decode legacy workflow args");
    assert_eq!(decoded.legacy_max_steps_per_input, Some(128));
    assert!(
        serde_json::to_value(decoded)
            .expect("re-serialize legacy workflow args")
            .get("max_steps_per_input")
            .is_none()
    );
}

#[test]
fn continuation_state_round_trips_admission_failure_correlation() {
    let rejection = engine::CommandRejection::context_revision_conflict(3, 4);
    let continuation = AgentSessionContinuationState::v1(vec![AgentAdmissionFailure {
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
    CoreAgentCommand::RequestRun(engine::RunRequestCommand {
        notify_on_terminal: Vec::new(),
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
        content: engine::ContentRef {
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
        metadata: Default::default(),
        universe_id: test_universe(),
        session_id: SessionId::new("session_test"),
        display_name: None,
        delete_after_close_ms: None,
        session_config: crate::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "gpt-test".to_owned(),
        }),
        workflow_tools: None,
        legacy_max_steps_per_input: None,
        continue_as_new_history_threshold: None,
        close_on_terminal,
        auto_reject_approvals: false,
        continuation_state: None,
    }
}

fn test_universe() -> uuid::Uuid {
    uuid::Uuid::from_u128(1)
}

fn managed_workflow_tools(controller_workflow_id: &str) -> engine::ManagedSessionWorkflowTools {
    engine::ManagedSessionWorkflowTools::v1(
        Some(engine::WorkflowEndpointRef {
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
        engine::EmissionEnvelope::run_terminal(
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
        command: engine::ResumeToolBatchCommand {
            run_id: RunId::new(1),
            batch_id: ToolBatchId::new(batch_id),
            claim: engine::WakeReason::Timeout,
            claim_observed_at_ms: 1_000,
            output: engine::ToolBatchResumeOutput::AwaitTool {
                result_ref: engine::BlobRef::from_bytes(b"await output"),
                additional_context: Vec::new(),
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

fn workflow_with_parked_tool_batch(spec: engine::AwaitSpec) -> AgentSessionWorkflow {
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
            promise_id_base: 1,
            calls: vec![engine::ToolCallState {
                call: engine::ObservedToolCall {
                    call_id: call_id.clone(),
                    tool_id: Some((engine::ToolName::new("await")).clone()),
                    tool_name: engine::ToolName::new("await"),
                    provider_kind: None,
                    arguments_ref: engine::BlobRef::from_bytes(b"{}"),
                    native_call_ref: None,
                },
                status: engine::ToolCallStatus::Pending,
                execution_policy: None,
                result: None,
            }],
        },
    );
    workflow.core_state.runs.active = Some(engine::ActiveRun {
        run_id,
        status: RunStatus::Parked,
        submission_id: None,
        source: engine::RunSource::Input {
            input: user_input(engine::BlobRef::from_bytes(b"start")),
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
        active_turn_id: None,
        active_tool_batch_id: Some(batch_id),
        approvals: Default::default(),
        parked_tool_batch: Some(engine::ParkedToolBatch {
            batch_id,
            suspension: engine::ToolBatchSuspension::AwaitTool { call_id, spec },
        }),
        tool_batches,
        completed_tool_batches: std::collections::BTreeMap::new(),
        output: None,
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
fn run_terminal_notifications_cannot_drop_output_encoding_into_a_session_promise() {
    let mut workflow = AgentSessionWorkflow::default();
    workflow.queue_emission(
        test_universe(),
        engine::EmissionEnvelope::run_terminal(
            test_universe(),
            SessionId::new("child_a"),
            EventSeq::new(8),
            "promise_1".to_owned(),
            RunId::new(1),
            RunStatus::Completed,
            Some(engine::ContentRef {
                content_ref: engine::BlobRef::from_bytes(b"native output"),
                media_type: Some("application/json".to_owned()),
                provider_kind: Some("provider.message".to_owned()),
            }),
            None,
        ),
    );
    assert!(workflow.pending_admissions.is_empty());
    assert!(workflow.pending_source_resolutions.is_empty());
    assert!(workflow.last_error.is_some());
}

#[test]
fn bound_dispatch_controls_push_delivery_independently_of_completion() {
    let mut workflow = AgentSessionWorkflow {
        universe_id: Some(test_universe()),
        session_id: Some(SessionId::new("child_session")),
        ..Default::default()
    };

    let binding = engine::WorkflowToolBinding::admit(
        test_universe(),
        engine::WorkflowToolDefinition {
            tool_id: engine::WorkflowToolId::new("approve"),
            revision: 1,
            semantic_type: "lightspeed.approval.request.v1".to_owned(),
            tool: engine::ToolSpec {
                name: engine::ToolName::new("request_approval"),
                execution: Default::default(),
                kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: engine::BlobRef::from_bytes(b"{}"),
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
                parallelism: engine::ToolParallelism::ParallelSafe,
            },
        },
        engine::WorkflowToolTarget::Bound {
            receiver: engine::WorkflowEndpointRef {
                workflow_id: "approval plugin id".to_owned(),
                workflow_kind: "approvals".to_owned(),
            },
            dispatch: engine::BoundWorkflowToolDispatch::Push,
        },
        engine::WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms: None,
            max_promises: 1,
            key_source: engine::WorkflowToolCompletionKeySource::Reply,
        },
    )
    .expect("binding");
    let invocation_id = engine::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        engine::TurnId::new(1),
        ToolBatchId::new(1),
        &engine::ToolCallId::new("call-1"),
        &binding.binding_fingerprint,
    );
    let invocation = engine::WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: binding.definition.tool_id.clone(),
        semantic_type: binding.definition.semantic_type.clone(),
        schema_revision: 1,
        binding_fingerprint: binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: engine::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: engine::ToolCallId::new("call-1"),
        arguments_ref: engine::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: Some(std::collections::BTreeMap::from([(
            engine::REPLY_COMPLETION_KEY.to_owned(),
            engine::PromiseId::from_number(1),
        )])),
    };
    workflow
        .core_state
        .workflow_tools
        .bindings
        .insert(binding.definition.tool_id.clone(), binding.clone());

    let entry = engine::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(9),
        },
        observed_at_ms: 100,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::WorkflowTool(engine::WorkflowToolEvent::Emitted {
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
        engine::EmissionBody::ToolInvocation {
            invocation: delivered,
            ..
        }
            if delivered == &invocation
    ));

    // Accepted completion uses the same push path when dispatch says Push.
    let accepted_binding = engine::WorkflowToolBinding::admit(
        test_universe(),
        binding.definition.clone(),
        engine::WorkflowToolTarget::Bound {
            receiver: engine::WorkflowEndpointRef {
                workflow_id: "approval plugin id".to_owned(),
                workflow_kind: "approvals".to_owned(),
            },
            dispatch: engine::BoundWorkflowToolDispatch::Push,
        },
        engine::WorkflowToolCompletion::Accepted,
    )
    .expect("pushed Accepted binding");
    let accepted_invocation_id = engine::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        engine::TurnId::new(1),
        ToolBatchId::new(1),
        &engine::ToolCallId::new("call-2"),
        &accepted_binding.binding_fingerprint,
    );
    let accepted_invocation = engine::WorkflowToolInvocation {
        invocation_id: accepted_invocation_id.clone(),
        tool_id: accepted_binding.definition.tool_id.clone(),
        semantic_type: accepted_binding.definition.semantic_type.clone(),
        schema_revision: accepted_binding.definition.revision,
        binding_fingerprint: accepted_binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: engine::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: engine::ToolCallId::new("call-2"),
        arguments_ref: engine::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: None,
    };
    workflow.core_state.workflow_tools.bindings.insert(
        accepted_binding.definition.tool_id.clone(),
        accepted_binding.clone(),
    );
    let accepted_entry = engine::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(10),
        },
        observed_at_ms: 101,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::WorkflowTool(engine::WorkflowToolEvent::Emitted {
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
        submission_digest: None,
        source: engine::RunSource::Input { input: Vec::new() },
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
    let terminal_entry = engine::CoreAgentEntry {
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
    let pull_binding = engine::WorkflowToolBinding::admit(
        test_universe(),
        binding.definition,
        engine::WorkflowToolTarget::Bound {
            receiver: engine::WorkflowEndpointRef {
                workflow_id: "approval plugin id".to_owned(),
                workflow_kind: "approvals".to_owned(),
            },
            dispatch: engine::BoundWorkflowToolDispatch::Pull,
        },
        engine::WorkflowToolCompletion::Accepted,
    )
    .expect("pull Accepted binding");
    let pull_invocation_id = engine::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        engine::TurnId::new(1),
        ToolBatchId::new(1),
        &engine::ToolCallId::new("call-3"),
        &pull_binding.binding_fingerprint,
    );
    let pull_invocation = engine::WorkflowToolInvocation {
        invocation_id: pull_invocation_id,
        tool_id: pull_binding.definition.tool_id.clone(),
        semantic_type: pull_binding.definition.semantic_type.clone(),
        schema_revision: pull_binding.definition.revision,
        binding_fingerprint: pull_binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: engine::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: engine::ToolCallId::new("call-3"),
        arguments_ref: engine::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: None,
    };
    workflow
        .core_state
        .workflow_tools
        .bindings
        .insert(pull_binding.definition.tool_id.clone(), pull_binding);
    let pull_entry = engine::CoreAgentEntry {
        position: SessionPosition {
            seq: EventSeq::new(12),
        },
        observed_at_ms: 103,
        joins: CoreAgentJoins::default(),
        event: CoreAgentEvent::WorkflowTool(engine::WorkflowToolEvent::Emitted {
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
    let mut workflow = AgentSessionWorkflow {
        universe_id: Some(test_universe()),
        session_id: Some(SessionId::new("child_session")),
        ..Default::default()
    };

    let start = engine::WorkflowStartRef {
        recipe_format: 1,
        revision: 1,
        recipe_ref: engine::BlobRef::from_bytes(b"recipe"),
        recipe_fingerprint: "wtr:sha256:recipe".to_owned(),
    };
    let binding = engine::WorkflowToolBinding::admit(
        test_universe(),
        engine::WorkflowToolDefinition {
            tool_id: engine::WorkflowToolId::new("launch"),
            revision: 1,
            semantic_type: "lightspeed.job.launch.v1".to_owned(),
            tool: engine::ToolSpec {
                name: engine::ToolName::new("launch_job"),
                execution: Default::default(),
                kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: engine::BlobRef::from_bytes(b"{}"),
                    output_schema_ref: None,
                    strict: None,
                    provider_options_ref: None,
                }),
                parallelism: engine::ToolParallelism::ParallelSafe,
            },
        },
        engine::WorkflowToolTarget::Start {
            start: start.clone(),
        },
        engine::WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms: None,
            max_promises: 1,
            key_source: engine::WorkflowToolCompletionKeySource::Reply,
        },
    )
    .expect("start binding");
    let invocation_id = engine::WorkflowToolInvocationId::for_call(
        test_universe(),
        &SessionId::new("child_session"),
        RunId::new(1),
        engine::TurnId::new(1),
        ToolBatchId::new(1),
        &engine::ToolCallId::new("call-1"),
        &binding.binding_fingerprint,
    );
    let promise_id = engine::PromiseId::from_number(1);
    let execution_id =
        engine::workflow_tool_execution_id(&invocation_id, &start.recipe_fingerprint);
    let invocation = engine::WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: binding.definition.tool_id.clone(),
        semantic_type: binding.definition.semantic_type.clone(),
        schema_revision: 1,
        binding_fingerprint: binding.binding_fingerprint.clone(),
        session_universe_id: test_universe(),
        session_id: SessionId::new("child_session"),
        run_id: RunId::new(1),
        turn_id: engine::TurnId::new(1),
        tool_batch_id: ToolBatchId::new(1),
        tool_call_id: engine::ToolCallId::new("call-1"),
        arguments_ref: engine::BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: Some(std::collections::BTreeMap::from([(
            engine::REPLY_COMPLETION_KEY.to_owned(),
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
        engine::Promise {
            promise_id: promise_id.clone(),
            source: engine::PromiseSource::Workflow {
                producer_workflow_id: execution_id.clone(),
                producer_workflow_kind: engine::WORKFLOW_TOOL_EXECUTION_KIND.to_owned(),
                invocation_id: invocation_id.as_str().to_owned(),
                completion_key: engine::REPLY_COMPLETION_KEY.to_owned(),
            },
            scope: engine::PromiseScope::Session,
            ownership: engine::PromiseOwnership::Model,
            status: PromiseStatus::Pending,
            payload_ref: None,
            error_ref: None,
            deadline_ms: None,
        },
    );

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
        .insert(invocation_id.clone(), engine::BlobRef::from_bytes(b"err"));
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

#[test]
fn terminal_run_with_notify_intent_queues_emission() {
    let mut workflow = AgentSessionWorkflow {
        universe_id: Some(test_universe()),
        session_id: Some(SessionId::new("child_session")),
        ..Default::default()
    };
    let output_ref = engine::ContentRef::text(engine::BlobRef::from_bytes(b"done"));
    workflow.core_state.runs.completed.push(RunRecord {
        run_id: RunId::new(3),
        status: RunStatus::Completed,
        submission_id: None,
        submission_digest: None,
        source: engine::RunSource::Input { input: Vec::new() },
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
    let entry = engine::CoreAgentEntry {
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
        engine::EmissionProducer::Session {
            universe_id,
            session_id,
            log_seq,
        } if *universe_id == test_universe()
            && session_id == &SessionId::new("child_session")
            && *log_seq == EventSeq::new(1)
    ));
    assert!(matches!(
        &pending.envelope.body,
        engine::EmissionBody::RunTerminal {
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

fn promise(id: &str, status: PromiseStatus) -> engine::Promise {
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
        completion_key: engine::REPLY_COMPLETION_KEY.to_owned(),
    }
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
        ownership: engine::PromiseOwnership::Model,
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
        deadline_at_ms,
    }
}

#[test]
fn workflow_await_waits_for_every_promise_in_all_mode() {
    let mut workflow = workflow_with_parked_tool_batch(await_spec(
        &["promise_1", "promise_2"],
        engine::AwaitMode::All,
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
    let promise_id = engine::PromiseId::new("promise_15");
    let mut workflow = workflow_with_parked_tool_batch(engine::AwaitSpec {
        promise_ids: vec![promise_id.clone()],
        mode: engine::AwaitMode::All,
        deadline_at_ms: None,
    });
    let invocation_id = engine::WorkflowToolInvocationId::for_call(
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
        .suspension = engine::ToolBatchSuspension::JoinedWorkflowCalls {
        calls: vec![engine::JoinedWorkflowCall {
            call_id: ToolCallId::new("call_await"),
            invocation_id,
            promise_id: promise_id.clone(),
        }],
        spec: engine::AwaitSpec {
            promise_ids: vec![promise_id.clone()],
            mode: engine::AwaitMode::All,
            deadline_at_ms: None,
        },
    };
    workflow.core_state.promises.promises.insert(
        promise_id.clone(),
        engine::Promise {
            promise_id,
            source: PromiseSource::Workflow {
                producer_workflow_id: "channels".to_owned(),
                producer_workflow_kind: "channels.session".to_owned(),
                invocation_id: "joined".to_owned(),
                completion_key: engine::REPLY_COMPLETION_KEY.to_owned(),
            },
            scope: PromiseScope::Run {
                run_id: RunId::new(1),
            },
            ownership: engine::PromiseOwnership::Runtime,
            status: PromiseStatus::Resolved,
            payload_ref: Some(engine::BlobRef::from_bytes(b"receipt")),
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
        engine::AwaitMode::Any,
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
        engine::AwaitMode::All,
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
fn promise_snapshot_reports_pending_promise() {
    let spec = await_spec(&["promise_1"], engine::AwaitMode::All, Some(1_000));
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
    let mut workflow = workflow_with_parked_tool_batch(engine::AwaitSpec {
        promise_ids: vec![
            engine::PromiseId::new("promise_11"),
            engine::PromiseId::new("promise_12"),
            engine::PromiseId::new("promise_13"),
            engine::PromiseId::new("promise_14"),
        ],
        mode: engine::AwaitMode::All,
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
fn continue_as_new_is_blocked_by_non_reconstructible_workflow_state() {
    let mut workflow = AgentSessionWorkflow::default();
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
        engine::PromiseId::new("promise_1"),
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
fn environment_catalog_switch_removal_replays_without_mutating_vfs() {
    fn append(
        state: &mut CoreAgentState,
        log: &mut Vec<CoreAgentEntry>,
        command: CoreAgentCommand,
    ) {
        for proposal in engine::admit_command(state, command, 1).unwrap() {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            engine::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.environments = Some(engine::EnvironmentsFeature {
        skills: Some(Default::default()),
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
            environment_id: engine::EnvironmentId::new("first"),
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
                    content: engine::ContentRef::text(BlobRef::from_bytes(b"menu")),
                    preview: None,
                    provenance_ref: None,
                    token_estimate: None,
                },
            },
        );
    }
    let vfs = engine::current_context_entry(&state, &vfs_key)
        .unwrap()
        .clone();
    let environment_publication = CoreAgentCommand::UpsertContext {
        expected_revision: None,
        key: env_key.clone(),
        entry: engine::current_catalog_inputs(&state)[&env_key].clone(),
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
        engine::current_context_entry(&disabled, &vfs_key),
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
            environment_id: engine::EnvironmentId::new("second"),
        },
    );
    let removal = drive::invalid_environment_catalog_command(&state).unwrap();
    assert!(matches!(&removal, CoreAgentCommand::RemoveContext { key, .. } if key == &env_key));
    assert!(!admissions::should_refresh_runtime_projection_before_admitting(&state, &removal));
    append(&mut state, &mut log, removal);
    assert!(engine::current_context_entry(&state, &env_key).is_none());
    assert_eq!(engine::current_context_entry(&state, &vfs_key), Some(&vfs));
    let mut replayed = CoreAgentState::new();
    for entry in &log {
        engine::apply_event(&mut replayed, entry).unwrap();
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
        for proposal in engine::admit_command(state, command, 1).unwrap() {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            engine::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.environments = Some(engine::EnvironmentsFeature {
        prompts: Some(Default::default()),
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
            environment_id: engine::EnvironmentId::new("first"),
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
                    content: engine::ContentRef::text(BlobRef::from_bytes(b"menu")),
                    preview: None,
                    provenance_ref: None,
                    token_estimate: None,
                },
            },
        );
    }
    let vfs = engine::current_context_entry(&state, &vfs_key)
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
        engine::current_context_entry(&disabled, &vfs_key),
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
            environment_id: engine::EnvironmentId::new("second"),
        },
    );
    let removal = drive::invalid_environment_prompt_command(&state).unwrap();
    assert!(matches!(&removal, CoreAgentCommand::RemoveContext { key, .. } if key == &env_key));
    assert!(!admissions::should_refresh_runtime_projection_before_admitting(&state, &removal));
    append(&mut state, &mut log, removal);
    assert!(engine::current_context_entry(&state, &env_key).is_none());
    assert_eq!(engine::current_context_entry(&state, &vfs_key), Some(&vfs));
    let mut replayed = CoreAgentState::new();
    for entry in &log {
        engine::apply_event(&mut replayed, entry).unwrap();
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
            environment_id: engine::EnvironmentId::new("machine"),
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
        let proposals = engine::admit_command(state, command, 1).unwrap();
        for proposal in proposals {
            let entry = CoreAgentEntry {
                position: SessionPosition {
                    seq: EventSeq::new(log.len() as u64 + 1),
                },
                observed_at_ms: 1,
                joins: proposal.joins,
                event: proposal.event,
            };
            engine::apply_event(state, &entry).unwrap();
            log.push(entry);
        }
    }
    let mut state = CoreAgentState::new();
    let mut log = Vec::new();
    let mut config = agent_session_args_with_close_on_terminal(false).session_config;
    config.features.vfs = Some(engine::VfsFeature {
        workspace_links: vec![engine::WorkspaceLink {
            path: "/skills".into(),
            target: engine::WorkspaceLinkTarget::Workspace {
                workspace_id: "skills".into(),
            },
            access: engine::WorkspaceLinkAccess::ReadOnly,
        }],
        skills: Some(engine::VfsSkillsConfig::default()),
        ..Default::default()
    });
    config.features.environments = Some(engine::EnvironmentsFeature {
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
        content: engine::ContentRef::text(BlobRef::from_bytes(b"menu")),
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
    let environment = engine::current_context_entry(&state, &env_key)
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
    assert!(engine::current_context_entry(&state, &key).is_none());
    assert_eq!(
        engine::current_context_entry(&state, &env_key),
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
        engine::apply_event(&mut replayed, entry).unwrap();
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
